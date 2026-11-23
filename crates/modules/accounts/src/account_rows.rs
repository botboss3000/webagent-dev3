//! The one writer of `user_accounts` rows.
//!
//! Every change to an account row raises its plain `auth_epoch` in the same statement, so a
//! process that remembered an identity notices the change on its next indexed read. Writing the
//! table anywhere else fails `account_rows_have_one_writer` (code search over the workspace).
use anyhow::{ensure, Result};
use sqlx::{Row, SqliteConnection, SqlitePool};

/// Adds the column to databases created before it existed; new rows start at 0.
pub(crate) async fn ensure_auth_epoch_column(pool: &SqlitePool) -> Result<()> {
    let columns = sqlx::query("PRAGMA table_info(user_accounts)")
        .fetch_all(pool)
        .await?;
    if !columns
        .iter()
        .any(|row| row.get::<String, _>("name") == "auth_epoch")
    {
        sqlx::query("ALTER TABLE user_accounts ADD COLUMN auth_epoch INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Replaces an existing row's record (and username when given) and raises its `auth_epoch`.
pub(crate) async fn update(
    conn: &mut SqliteConnection,
    user_id: &str,
    username: Option<&str>,
    sealed: &[u8],
) -> Result<()> {
    let changed = sqlx::query(
        "UPDATE user_accounts SET value=?,username=COALESCE(?,username),auth_epoch=auth_epoch+1 \
         WHERE user_id=?",
    )
    .bind(sealed)
    .bind(username)
    .bind(user_id)
    .execute(conn)
    .await?
    .rows_affected();
    ensure!(changed == 1, "Account not found");
    Ok(())
}

/// Takes the database write lock on the row without changing it (no epoch change).
pub(crate) async fn lock(conn: &mut SqliteConnection, user_id: &str) -> Result<()> {
    sqlx::query("UPDATE user_accounts SET value=value WHERE user_id=?")
        .bind(user_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Creates a row; `or_ignore` keeps an existing row unchanged (startup import).
pub(crate) async fn insert(
    conn: &mut SqliteConnection,
    user_id: &str,
    username: &str,
    sealed: &[u8],
    or_ignore: bool,
) -> Result<()> {
    let statement = if or_ignore {
        "INSERT OR IGNORE INTO user_accounts(user_id,username,value) VALUES(?,?,?)"
    } else {
        "INSERT INTO user_accounts(user_id,username,value) VALUES(?,?,?)"
    };
    sqlx::query(statement)
        .bind(user_id)
        .bind(username)
        .bind(sealed)
        .execute(conn)
        .await?;
    Ok(())
}

/// The account's current `auth_epoch`; `None` when the row or its value is missing, which
/// callers treat as changed (fail closed).
pub(crate) async fn auth_epoch(pool: &SqlitePool, user_id: &str) -> Result<Option<i64>> {
    let value: Option<Option<i64>> =
        sqlx::query_scalar("SELECT auth_epoch FROM user_accounts WHERE user_id=?")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(value.flatten())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// Statements that write `user_accounts`; only this file may contain them outside tests.
    const WRITES: [&str; 5] = [
        "UPDATE user_accounts",
        "DELETE FROM user_accounts",
        "INSERT INTO user_accounts",
        "INSERT OR IGNORE INTO user_accounts",
        "REPLACE INTO user_accounts",
    ];

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !matches!(name.as_str(), "target" | "tests" | ".git" | "node_modules") {
                    rust_files(&path, out);
                }
            } else if name.ends_with(".rs") {
                out.push(path);
            }
        }
    }

    /// The file without its test-only code: test files are skipped whole, and each item marked
    /// `#[cfg(test)]` (an inline `mod tests { .. }`, a helper `fn`, a `mod x;` line) is removed.
    fn production_part(path: &Path, text: &str) -> Option<String> {
        let name = path.file_name()?.to_string_lossy();
        if name == "tests.rs" || name.ends_with("_tests.rs") || name == "fixtures.rs" {
            return None;
        }
        let mut kept = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("#[cfg(test)]") {
            kept.push_str(&rest[..start]);
            let item = &rest[start + "#[cfg(test)]".len()..];
            rest = match (item.find(';'), item.find('{')) {
                (Some(semi), Some(open)) if semi < open => &item[semi + 1..],
                (Some(semi), None) => &item[semi + 1..],
                (_, Some(open)) => {
                    let mut depth = 0usize;
                    let mut end = item.len();
                    for (offset, byte) in item[open..].bytes().enumerate() {
                        match byte {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = open + offset + 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    &item[end..]
                }
                (None, None) => "",
            };
        }
        kept.push_str(rest);
        Some(kept)
    }

    #[test]
    fn test_items_are_removed_and_production_code_kept() {
        let text = "#[cfg(test)]\nmod a;\nfn keep1() {}\n#[cfg(test)]\nmod tests {\n fn x() { if y { } }\n}\nfn keep2() {}\n";
        let kept = production_part(Path::new("lib.rs"), text).unwrap();
        assert!(kept.contains("keep1") && kept.contains("keep2"));
        assert!(!kept.contains("fn x"));
    }

    #[test]
    fn account_rows_have_one_writer() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.join("../../..");
        let own = manifest.join("src/account_rows.rs").canonicalize().unwrap();
        let mut files = Vec::new();
        for dir in ["src", "crates", "abilities"] {
            rust_files(&root.join(dir), &mut files);
        }
        assert!(files.len() > 100, "workspace sources not found");
        let mut offenders = Vec::new();
        for path in files {
            if path.canonicalize().ok().as_deref() == Some(own.as_path()) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let Some(code) = production_part(&path, &text) else {
                continue;
            };
            // Statements may be split over lines: compare with whitespace collapsed.
            let code = code.split_whitespace().collect::<Vec<_>>().join(" ");
            if WRITES.iter().any(|write| code.contains(write)) {
                offenders.push(path.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "user_accounts written outside account_rows.rs (auth_epoch would not move): {offenders:?}"
        );
    }
}
