use anyhow::{bail, Result};
use sqlx::PgPool;

use crate::auth::hash_password;

pub async fn create_user(pool: &PgPool, username: &str, password: &str) -> Result<()> {
    let username = protocol::validate_username(username).map_err(anyhow::Error::msg)?;
    protocol::validate_password(password).map_err(anyhow::Error::msg)?;
    let hash = hash_password(password)?;
    let inserted = sqlx::query("INSERT INTO users (username, password_hash) VALUES ($1, $2)")
        .bind(&username)
        .bind(hash)
        .execute(pool)
        .await;
    match inserted {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => bail!("user {username} already exists"),
        other => other.map(|_| ()).map_err(Into::into),
    }
}

/// Sets a new password and signs the user out everywhere.
pub async fn reset_password(pool: &PgPool, username: &str, password: &str) -> Result<()> {
    let username = protocol::validate_username(username).map_err(anyhow::Error::msg)?;
    protocol::validate_password(password).map_err(anyhow::Error::msg)?;
    let hash = hash_password(password)?;
    let mut tx = pool.begin().await?;
    let id: Option<(i64,)> = sqlx::query_as("UPDATE users SET password_hash = $1 WHERE username = $2 RETURNING id")
        .bind(hash)
        .bind(&username)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((id,)) = id else { bail!("no such user: {username}") };
    sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Prompts twice on the terminal so the password never lands in shell history.
pub fn prompt_new_password() -> Result<String> {
    use std::io::IsTerminal;
    // Piped input (scripts, `docker compose exec -T`) supplies the password twice, one per line.
    let (first, second) = if std::io::stdin().is_terminal() {
        (rpassword::prompt_password("New password: ")?, rpassword::prompt_password("Repeat password: ")?)
    } else {
        use std::io::BufRead;
        let mut input = std::io::stdin().lock();
        let mut line = || -> Result<String> {
            let mut text = String::new();
            input.read_line(&mut text)?;
            Ok(text.trim_end_matches(['\r', '\n']).to_owned())
        };
        (line()?, line()?)
    };
    if first != second {
        bail!("passwords do not match");
    }
    Ok(first)
}
