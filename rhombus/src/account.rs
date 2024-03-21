use axum::{extract::State, http::Uri, response::Html, Extension};
use cached::proc_macro::cached;
use minijinja::context;
use reqwest::Client;
use tracing::info;

use crate::{auth::ClientUser, RhombusRouterState};

#[cached(time = 10, key = "String", convert = "{ discord_id.to_string() }")]
async fn is_in_server(discord_guild_id: &str, discord_id: &str, discord_bot_token: &str) -> bool {
    let client = Client::new();
    let res = client
        .get(format!(
            "https://discord.com/api/guilds/{}/members/{}",
            discord_guild_id, discord_id
        ))
        .header("Authorization", format!("Bot {}", discord_bot_token))
        .send()
        .await
        .unwrap();

    res.status().is_success()
}

pub async fn route_account(
    State(state): State<RhombusRouterState>,
    Extension(user): Extension<ClientUser>,
    uri: Uri,
) -> Html<String> {
    let val = is_in_server(
        &state.config.discord_guild_id,
        &user.discord_id,
        &state.config.discord_bot_token,
    )
    .await;

    info!("is_in_server: {}", val);

    Html(
        state
            .jinja
            .get_template("account.html")
            .unwrap()
            .render(context! {
                user => user,
                uri => uri.to_string(),
                location_url => state.config.location_url,
                og_image => format!("{}/og-image.png", state.config.location_url)
            })
            .unwrap(),
    )
}
