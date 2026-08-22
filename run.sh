#!/usr/bin/env bash
# 本机开发入口：构建、启动看板、清理产物。任意目录执行均可。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

BIND_DEFAULT="127.0.0.1:3847"
VITE_URL="http://127.0.0.1:5173"

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
  build [release]      构建 Web UI 与采集/看板二进制（默认 debug）
  run   [release] [...] 启动看板；其余参数转给 ai-usage-dash serve
  dev                  看板 API + Vite 热更新（改 UI 不必重编二进制）
  agent [release] [...] 转发给 ai-usage-agent（缺二进制时先编）
  dash  [release] [...] 转发给 ai-usage-dash
  test  [...]          cargo test --workspace；其余参数原样转发
  clean [all]          清理 Rust / Web 产物；all 含 node_modules

示例:
  ./run.sh build
  ./run.sh run
  ./run.sh run --bind 127.0.0.1:3847
  ./run.sh dev
  ./run.sh agent init --url http://127.0.0.1:3847 --token <token>
  ./run.sh clean
EOF
}

bin_path() {
  local name="$1" profile="$2"
  if [[ "$profile" == release ]]; then
    printf '%s/target/release/%s' "$ROOT" "$name"
  else
    printf '%s/target/debug/%s' "$ROOT" "$name"
  fi
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
  [[ "$profile" == release ]] && args+=(--release)
  local p
  for p in "$@"; do
    args+=(-p "$p")
  done
  log "cargo build ${args[*]}"
  cargo build "${args[@]}"
}

port_open() {
  local host="$1" port="$2"
  (echo >/dev/tcp/"$host"/"$port") >/dev/null 2>&1
}

cmd_build() {
  take_profile "$@"
  if [[ ${#REST[@]} -gt 0 ]]; then
    die "未知参数: ${REST[*]}（可用: release）"
  fi
  web_build
  cargo_bins "$PROFILE" ai-usage-dash ai-usage-agent
  log "完成 → $(bin_path ai-usage-dash "$PROFILE")"
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

cmd_agent() {
  take_profile "$@"
  cargo_bins "$PROFILE" ai-usage-agent
  exec "$(bin_path ai-usage-agent "$PROFILE")" "${REST[@]}"
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
