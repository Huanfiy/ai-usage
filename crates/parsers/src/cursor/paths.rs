use std::path::{Path, PathBuf};

use crate::util::expand_home;
use crate::ParseCtx;

const STATE_REL: &str = "User/globalStorage/state.vscdb";

/// First existing `state.vscdb`. Defaults stay under `ctx.home` so tests with a
/// fixture home cannot accidentally open the real Cursor DB via process env.
pub fn detect_state_db(ctx: &ParseCtx) -> Option<PathBuf> {
    candidates(ctx).into_iter().find(|p| p.is_file())
}

fn candidates(ctx: &ParseCtx) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(p) = &ctx.env.cursor_state_db {
        push(p.clone());
    }
    if let Ok(p) = std::env::var("CURSOR_STATE_DB_PATH") {
        if !p.trim().is_empty() {
            push(expand_home(p.trim(), &ctx.home));
        }
    }
    push(platform_default(&ctx.home));
    if let Some(p) = linux_xdg_for_real_home(ctx) {
        push(p);
    }
    out
}

fn platform_default(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home
            .join("Library/Application Support/Cursor")
            .join(STATE_REL);
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"));
        return appdata.join("Cursor").join(STATE_REL);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".config").join("Cursor").join(STATE_REL)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn linux_xdg_for_real_home(ctx: &ParseCtx) -> Option<PathBuf> {
    if !is_real_home(&ctx.home) {
        return None;
    }
    let xdg = std::env::var("XDG_CONFIG_HOME").ok()?;
    if xdg.trim().is_empty() {
        return None;
    }
    Some(PathBuf::from(xdg).join("Cursor").join(STATE_REL))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn linux_xdg_for_real_home(_ctx: &ParseCtx) -> Option<PathBuf> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn is_real_home(home: &Path) -> bool {
    let Some(env_home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    else {
        return false;
    };
    let env_home = PathBuf::from(env_home);
    match (home.canonicalize(), env_home.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => home == env_home,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ParseCtx;
    use std::fs;

    #[test]
    fn detect_uses_env_override() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.vscdb");
        fs::write(&db, b"").unwrap();
        let ctx = ParseCtx {
            home: dir.path().join("not-home"),
            cache_dir: dir.path().join("cache"),
            env: crate::AdapterEnv {
                cursor_state_db: Some(db.clone()),
                ..crate::AdapterEnv::default()
            },
        };
        assert_eq!(detect_state_db(&ctx).as_deref(), Some(db.as_path()));
    }

    #[test]
    fn fixture_home_does_not_use_process_xdg() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ParseCtx::new(dir.path().to_path_buf(), dir.path().join("cache"));
        assert!(detect_state_db(&ctx).is_none());
    }
}
