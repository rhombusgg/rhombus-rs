#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .event_format(
            tracing_subscriber::fmt::format()
                .with_file(true)
                .with_line_number(true),
        )
        .init();

    dotenvy::dotenv().unwrap();

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect("postgres://postgres:password@localhost")
        .await
        .unwrap();

    let app = rhombus::Rhombus::new(
        pool,
        rhombus::Config {
            location_url: "http://localhost:3000".to_string(),
            discord_client_id: std::env::var("DISCORD_CLIENT_ID").unwrap(),
            discord_client_secret: std::env::var("DISCORD_CLIENT_SECRET").unwrap(),
            discord_bot_token: std::env::var("DISCORD_TOKEN").unwrap(),
            discord_guild_id: std::env::var("DISCORD_GUILD_ID").unwrap(),
            jwt_secret: std::env::var("JWT_SECRET").unwrap(),
        },
    )
    .build()
    .await;

    rhombus::serve(app, "0.0.0.0:3000").await.unwrap();
}