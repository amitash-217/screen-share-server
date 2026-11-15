use std::net::SocketAddr;

use anyhow::Result;
use clap::{Parser, Subcommand};
use server::admin;
use server::api::{router, AppState};
use server::rooms;
use server::turn::TurnConfig;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the API and signaling server.
    Serve {
        #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:8080")]
        bind: SocketAddr,
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,
        /// Set SIGNUP_ENABLED=false to close open signup.
        #[arg(long, env = "SIGNUP_ENABLED", default_value_t = true, action = clap::ArgAction::Set)]
        signup_enabled: bool,
        /// Shared secret of the coturn server. Without it, clients get no TURN servers.
        #[arg(long, env = "TURN_SECRET")]
        turn_secret: Option<String>,
        /// Public IP or domain of this machine, advertised as the TURN host.
        #[arg(long, env = "PUBLIC_HOST")]
        public_host: Option<String>,
        /// Set when a reverse proxy in front of the server is the only way in: rate limits then
        /// count the address in X-Forwarded-For instead of the proxy's.
        #[arg(long, env = "TRUST_PROXY", default_value_t = false, action = clap::ArgAction::Set)]
        trust_proxy: bool,
    },
    /// Create an account. Needed when signup is closed.
    CreateUser {
        #[arg(env = "DATABASE_URL", long)]
        database_url: String,
        username: String,
    },
    /// Set a new password for a user who forgot theirs. Signs them out everywhere.
    ResetPassword {
        #[arg(env = "DATABASE_URL", long)]
        database_url: String,
        username: String,
    },
}

async fn connect(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new().max_connections(10).connect(database_url).await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    match Cli::parse().command {
        Command::Serve { bind, database_url, signup_enabled, turn_secret, public_host, trust_proxy } => {
            let pool = connect(&database_url).await?;
            let purged = rooms::purge_temporary(&pool).await?;
            tracing::info!(%bind, signup_enabled, purged_temporary_rooms = purged, "starting server");
            let mut state = AppState::new(pool, signup_enabled);
            if trust_proxy {
                state = state.with_trusted_proxy();
            }
            match (turn_secret, public_host) {
                (Some(secret), Some(host)) => state = state.with_turn(TurnConfig { secret, host }),
                _ => tracing::warn!("TURN_SECRET and PUBLIC_HOST not both set: clients get no TURN servers"),
            }
            let listener = tokio::net::TcpListener::bind(bind).await?;
            axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await?;
        }
        Command::CreateUser { database_url, username } => {
            let pool = connect(&database_url).await?;
            admin::create_user(&pool, &username, &admin::prompt_new_password()?).await?;
            println!("created user {username}");
        }
        Command::ResetPassword { database_url, username } => {
            let pool = connect(&database_url).await?;
            admin::reset_password(&pool, &username, &admin::prompt_new_password()?).await?;
            println!("password reset for {username}; all sessions ended");
        }
    }
    Ok(())
}
