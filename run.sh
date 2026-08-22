#!/usr/bin/env bash
# 本机开发入口：构建、启动看板、清理产物。任意目录执行均可。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

BIND_DEFAULT="127.0.0.1:3847"
AGENT_PANEL_DEFAULT="127.0.0.1:3848"
AGENT_SERVICE="ai-usage-agent.service"
VITE_URL="http://127.0.0.1:5173"
MUSL_TARGET="x86_64-unknown-linux-musl"

log() { printf '==> %s\n' "$*"; }
err() { printf 'run.sh: %s\n' "$*" >&2; }
die() { err "$*"; exit 1; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "未找到命令: $1"
}

usage() {
  cat <<'EOF'
用法: ./run.sh <命令> [参数]

命令:
  build [release|musl] 构建 Web UI 与采集/看板二进制（默认 debug；musl 为静态发布）
  run   [release] [...] 启动看板；端口已被本看板占用则先结束再启动
  dev                  看板 API + Vite 热更新（改 UI 不必重编二进制）
  agent [release] [...] 转发给 ai-usage-agent（缺二进制时先编）
  agent [release] reload
                       编采集端、装到 user service 路径并重启
                       （已有 service 时用这个，不要再开前台 daemon）
  panel                打开采集端本机面板（默认 http://127.0.0.1:3848）
  dash  [release] [...] 转发给 ai-usage-dash
  test  [...]          cargo test --workspace；其余参数原样转发
  clean [all]          清理构建产物；all 含 node_modules

示例:
  ./run.sh build
  ./run.sh build musl
  ./run.sh run
  ./run.sh run --bind 127.0.0.1:3847
  ./run.sh dev
  ./run.sh agent init --url http://127.0.0.1:3847 --token <token>
  ./run.sh agent reload
  ./run.sh panel
  ./run.sh clean
EOF
}

bin_path() {
  local name="$1" profile="$2"
  case "$profile" in
    musl) printf '%s/target/%s/release/%s' "$ROOT" "$MUSL_TARGET" "$name" ;;
    release) printf '%s/target/release/%s' "$ROOT" "$name" ;;
    *) printf '%s/target/debug/%s' "$ROOT" "$name" ;;
  esac
}

take_profile() {
  PROFILE=debug
  if [[ "${1:-}" == "release" || "${1:-}" == "--release" ]]; then
    PROFILE=release
    shift
  fi
  REST=("$@")
}

web_install() {
  need_cmd npm
  if [[ ! -d "$ROOT/web/node_modules" ]]; then
    log "安装 Web 依赖"
    (cd "$ROOT/web" && npm ci)
  fi
}

web_build() {
  web_install
  log "构建 Web UI"
  (cd "$ROOT/web" && npm run build)
}

cargo_bins() {
  local profile="$1"
  shift
  need_cmd cargo
  local args=()
  if [[ "$profile" == musl ]]; then
    need_cmd rustup
    need_cmd musl-gcc
    if ! rustup target list --installed | grep -qx "$MUSL_TARGET"; then
      die "未安装 musl target。先执行: rustup target add $MUSL_TARGET"
    fi
    export CC_x86_64_unknown_linux_musl=musl-gcc
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc
    export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+crt-static"
    args+=(--release --target "$MUSL_TARGET")
  elif [[ "$profile" == release ]]; then
    args+=(--release)
  fi
  local p
  for p in "$@"; do
    args+=(-p "$p")
  done
  log "cargo build ${args[*]}"
  cargo build "${args[@]}"
}

assert_static() {
  local bin="$1" info ldd_out
  [[ -x "$bin" ]] || die "未找到二进制: $bin"
  info="$(file -b "$bin" 2>/dev/null || file "$bin")"
  log "$bin: $info"
  if ! command -v ldd >/dev/null 2>&1; then
    return 0
  fi
  ldd_out="$(ldd "$bin" 2>&1 || true)"
  if grep -qiE 'not a dynamic executable|statically linked' <<<"$ldd_out"; then
    return 0
  fi
  err "$ldd_out"
  die "不是静态二进制: $bin"
}

port_open() {
  local host="$1" port="$2"
  (echo >/dev/tcp/"$host"/"$port") >/dev/null 2>&1
}

run_bind() {
  local bind="$BIND_DEFAULT" prev="" a
  for a in "$@"; do
    if [[ "$prev" == "--bind" ]]; then
      bind="$a"
      prev=""
      continue
    fi
    case "$a" in
      --bind=*) bind="${a#--bind=}" ;;
      --bind) prev="--bind" ;;
    esac
  done
  printf '%s\n' "$bind"
}

listen_pids() {
  local port="$1"
  {
    if command -v ss >/dev/null 2>&1; then
      ss -ltnp "sport = :${port}" 2>/dev/null | grep -oE 'pid=[0-9]+' | cut -d= -f2 || true
    fi
    if command -v lsof >/dev/null 2>&1; then
      lsof -t -iTCP:"$port" -sTCP:LISTEN 2>/dev/null || true
    fi
  } | awk 'NF && !seen[$0]++'
}

proc_cmdline() {
  local pid="$1"
  if [[ -r "/proc/${pid}/cmdline" ]]; then
    tr '\0' ' ' <"/proc/${pid}/cmdline"
  else
    ps -o args= -p "$pid" 2>/dev/null || true
  fi
}

stop_dash_on_bind() {
  local bind="$1"
  local host="${bind%:*}"
  local port="${bind##*:}"
  if [[ "$bind" != *:* || -z "$host" || -z "$port" ]]; then
    return 0
  fi
  if ! port_open "$host" "$port"; then
    return 0
  fi

  local pid dash_pids=() other_pids=()
  while read -r pid; do
    [[ -z "$pid" ]] && continue
    if [[ "$(proc_cmdline "$pid")" == *ai-usage-dash* ]]; then
      dash_pids+=("$pid")
    else
      other_pids+=("$pid")
    fi
  done < <(listen_pids "$port")

  if [[ ${#dash_pids[@]} -eq 0 ]]; then
    if [[ ${#other_pids[@]} -gt 0 ]]; then
      die "无法重启：${bind} 被其他进程占用（pid ${other_pids[*]}）"
    fi
    die "无法重启：${bind} 已被占用"
  fi

  log "结束已占用 ${bind} 的看板（pid ${dash_pids[*]}）"
  for pid in "${dash_pids[@]}"; do
    kill -INT "$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
  done

  local i alive
  for i in $(seq 1 50); do
    alive=0
    for pid in "${dash_pids[@]}"; do
      if kill -0 "$pid" 2>/dev/null; then
        alive=1
        break
      fi
    done
    if [[ "$alive" -eq 0 ]] && ! port_open "$host" "$port"; then
      return 0
    fi
    sleep 0.1
  done
  for pid in "${dash_pids[@]}"; do
    kill -KILL "$pid" 2>/dev/null || true
  done
  sleep 0.1
  if port_open "$host" "$port"; then
    die "无法释放 ${bind}"
  fi
}

cmd_build() {
  local profile=debug
  case "${1:-}" in
    "") ;;
    release|--release) profile=release ;;
    musl) profile=musl ;;
    *) die "未知参数: $1（可用: release | musl）" ;;
  esac
  if [[ $# -gt 1 ]]; then
    die "未知参数: $*（可用: release | musl）"
  fi
  web_build
  cargo_bins "$profile" ai-usage-dash ai-usage-agent
  if [[ "$profile" == musl ]]; then
    assert_static "$(bin_path ai-usage-dash musl)"
    assert_static "$(bin_path ai-usage-agent musl)"
  fi
  log "完成 → $(bin_path ai-usage-dash "$profile")"
}

cmd_run() {
  local profile=debug extras=()
  local arg
  for arg in "$@"; do
    case "$arg" in
      --release|release) profile=release ;;
      *) extras+=("$arg") ;;
    esac
  done
  web_install
  if [[ ! -f "$ROOT/web/dist/index.html" ]]; then
    web_build
  fi
  cargo_bins "$profile" ai-usage-dash
  stop_dash_on_bind "$(run_bind "${extras[@]}")"
  log "启动看板（Ctrl+C 结束）"
  exec "$(bin_path ai-usage-dash "$profile")" serve "${extras[@]}"
}

cmd_dev() {
  if [[ $# -gt 0 ]]; then
    die "dev 不接受参数"
  fi
  web_install
  cargo_bins debug ai-usage-dash

  local dash_pid=""
  cleanup() {
    trap - EXIT INT TERM
    if [[ -n "$dash_pid" ]] && kill -0 "$dash_pid" 2>/dev/null; then
      kill "$dash_pid" 2>/dev/null || true
      wait "$dash_pid" 2>/dev/null || true
    fi
  }

  if port_open 127.0.0.1 3847; then
    log "复用已在监听的看板 API  http://${BIND_DEFAULT}"
  else
    mkdir -p "$ROOT/tmp"
    local logf="$ROOT/tmp/dash-dev.log"
    "$(bin_path ai-usage-dash debug)" serve --bind "$BIND_DEFAULT" >"$logf" 2>&1 &
    dash_pid=$!
    trap cleanup EXIT
    trap 'cleanup; exit 130' INT TERM
    sleep 0.2
    if ! kill -0 "$dash_pid" 2>/dev/null; then
      cat "$logf" >&2 || true
      die "看板启动失败，日志: $logf"
    fi
    log "看板 API  http://${BIND_DEFAULT}  （日志 $logf）"
  fi

  log "Vite UI   ${VITE_URL}"
  (cd "$ROOT/web" && npm run dev)
}

agent_config_path() {
  if [[ -n "${XDG_CONFIG_HOME:-}" ]]; then
    printf '%s\n' "$XDG_CONFIG_HOME/ai-usage/agent.toml"
  else
    printf '%s\n' "$HOME/.config/ai-usage/agent.toml"
  fi
}

agent_panel_bind() {
  local toml bind=""
  toml="$(agent_config_path)"
  if [[ -f "$toml" ]]; then
    bind="$(awk -F'=' '
      $1 ~ /^[ \t]*bind[ \t]*$/ {
        v=$2
        gsub(/^[ \t"]+|[ \t"]+$/, "", v)
        print v
        exit
      }
    ' "$toml")"
  fi
  if [[ -z "$bind" ]]; then
    bind="$AGENT_PANEL_DEFAULT"
  fi
  printf '%s\n' "$bind"
}

agent_service_bin() {
  local show path=""
  if command -v systemctl >/dev/null 2>&1; then
    show="$(systemctl --user show -p ExecStart --value "$AGENT_SERVICE" 2>/dev/null || true)"
    if [[ "$show" =~ path=([^[:space:];]+) ]]; then
      path="${BASH_REMATCH[1]}"
    fi
  fi
  if [[ -z "$path" ]]; then
    path="$HOME/.local/bin/ai-usage-agent"
  fi
  printf '%s\n' "$path"
}

agent_service_present() {
  command -v systemctl >/dev/null 2>&1 || return 1
  systemctl --user is-enabled "$AGENT_SERVICE" >/dev/null 2>&1 \
    || systemctl --user is-active "$AGENT_SERVICE" >/dev/null 2>&1
}

install_agent_bin() {
  local src dst dir tmp
  src="$(bin_path ai-usage-agent "$PROFILE")"
  dst="$(agent_service_bin)"
  [[ -x "$src" ]] || die "未找到二进制: $src"
  if [[ "$(basename "$dst")" != "ai-usage-agent" ]]; then
    die "拒绝覆盖非采集端路径: $dst"
  fi
  dir="$(dirname "$dst")"
  mkdir -p "$dir"
  tmp="${dst}.new.$$"
  cp "$src" "$tmp"
  chmod 755 "$tmp"
  mv "$tmp" "$dst"
  log "已安装 $dst"
}

wait_panel() {
  local bind="$1"
  local host="${bind%:*}"
  local port="${bind##*:}"
  local i
  for i in $(seq 1 50); do
    if port_open "$host" "$port"; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

cmd_agent_reload() {
  cargo_bins "$PROFILE" ai-usage-agent
  install_agent_bin
  if agent_service_present; then
    log "重启 $AGENT_SERVICE"
    systemctl --user restart "$AGENT_SERVICE"
    if ! wait_panel "$(agent_panel_bind)"; then
      err "service 已重启，但面板尚未监听 $(agent_panel_bind)"
      systemctl --user --no-pager --full status "$AGENT_SERVICE" >&2 || true
      exit 1
    fi
    log "面板  http://$(agent_panel_bind)"
  else
    log "未安装 user service。前台：./run.sh agent daemon"
    log "安装 service：./run.sh agent daemon install"
  fi
}

cmd_agent_panel() {
  if [[ $# -gt 0 ]]; then
    die "panel 不接受参数"
  fi
  local bind url
  bind="$(agent_panel_bind)"
  url="http://${bind}"
  local host="${bind%:*}"
  local port="${bind##*:}"
  if ! port_open "$host" "$port"; then
    die "面板未在监听 ${bind}。已有 service 时用 ./run.sh agent reload，否则 ./run.sh agent daemon"
  fi
  log "$url"
  if command -v xdg-open >/dev/null 2>&1 && [[ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]]; then
    xdg-open "$url" >/dev/null 2>&1 || true
  fi
}

cmd_agent() {
  take_profile "$@"
  local sub="${REST[0]:-}"
  case "$sub" in
    reload)
      if [[ ${#REST[@]} -gt 1 ]]; then
        die "未知参数: ${REST[*]:1}（可用: ./run.sh agent reload  或  ./run.sh agent release reload）"
      fi
      cmd_agent_reload
      ;;
    panel)
      if [[ ${#REST[@]} -gt 1 ]]; then
        die "未知参数: ${REST[*]:1}"
      fi
      cmd_agent_panel
      ;;
    *)
      cargo_bins "$PROFILE" ai-usage-agent
      exec "$(bin_path ai-usage-agent "$PROFILE")" "${REST[@]}"
      ;;
  esac
}

cmd_dash() {
  take_profile "$@"
  cargo_bins "$PROFILE" ai-usage-dash
  exec "$(bin_path ai-usage-dash "$PROFILE")" "${REST[@]}"
}

cmd_test() {
  need_cmd cargo
  cargo test --workspace "$@"
}

cmd_clean() {
  local all=0
  case "${1:-}" in
    "") ;;
    all|--all) all=1 ;;
    *) die "未知参数: $1（可用: all）" ;;
  esac
  if command -v cargo >/dev/null 2>&1; then
    log "cargo clean"
    cargo clean
  fi
  log "删除 web/dist crates/dash/web-dist"
  rm -rf "$ROOT/web/dist" "$ROOT/crates/dash/web-dist"
  if [[ "$all" -eq 1 ]]; then
    log "删除 web/node_modules"
    rm -rf "$ROOT/web/node_modules"
  fi
}

main() {
  local cmd="${1:-}"
  shift || true
  case "$cmd" in
    ""|-h|--help|help) usage ;;
    build) cmd_build "$@" ;;
    run) cmd_run "$@" ;;
    dev) cmd_dev "$@" ;;
    agent) cmd_agent "$@" ;;
    panel) cmd_agent_panel "$@" ;;
    dash) cmd_dash "$@" ;;
    test) cmd_test "$@" ;;
    clean) cmd_clean "$@" ;;
    *)
      err "未知命令: $cmd"
      usage >&2
      exit 1
      ;;
  esac
}

main "$@"
