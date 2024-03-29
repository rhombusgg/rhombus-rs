#![forbid(unsafe_code)]

pub mod account;
pub mod auth;
pub mod challenges;
pub mod command_palette;
pub mod locales;
pub mod open_graph;
pub mod plugin;
pub mod track;

use axum::{
    extract::State,
    http::{StatusCode, Uri},
    middleware,
    response::{Html, IntoResponse},
    routing::get,
    Extension, Router,
};
use minijinja::{context, Environment};
use sqlx::PgPool;
use std::{net::SocketAddr, sync::Arc};
use tokio::net::{TcpListener, ToSocketAddrs};
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::{compression::CompressionLayer, services::ServeDir};
use tracing::info;

use account::route_account;
use auth::{
    auth_injector_middleware, enforce_auth_middleware, route_discord_callback, route_signin,
    route_signout, MaybeClientUser,
};
use challenges::route_challenges;
use command_palette::route_command_palette;
use locales::{translate, Lang};
use open_graph::route_default_og_image;
use plugin::Plugin;
use track::track;

use crate::locales::Localizations;

pub struct Rhombus<'a> {
    db: PgPool,
    config: Config,
    plugins: Vec<&'a (dyn Plugin + Sync)>,
}

#[derive(Clone)]
pub struct Config {
    pub jwt_secret: String,
    pub discord_client_id: String,
    pub discord_client_secret: String,
    pub discord_bot_token: String,
    pub discord_guild_id: String,
    pub location_url: String,
}

pub type RhombusRouterState = Arc<RhombusRouterStateInner>;

pub struct RhombusRouterStateInner {
    pub db: PgPool,
    pub jinja: Environment<'static>,
    pub config: Config,
    pub discord_signin_url: String,
}

async fn handler_404() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Html("404"))
}

static STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

impl<'a> Rhombus<'a> {
    pub fn new(db: PgPool, config: Config) -> Self {
        Self {
            db,
            plugins: Vec::new(),
            config,
        }
    }

    pub fn plugin(self, plugin: &'a (impl Plugin + Sync)) -> Self {
        let mut plugins = self.plugins;
        plugins.push(plugin);

        Self {
            db: self.db,
            plugins,
            config: self.config,
        }
    }

    pub async fn build(&self) -> Router {
        sqlx::migrate!().run(&self.db).await.unwrap();

        for plugin in self.plugins.clone().iter() {
            plugin.migrate(self.db.clone()).await;
        }

        let mut localizer = Localizations::new();

        for plugin in self.plugins.clone().iter() {
            plugin.localize(&mut localizer.bundles);
        }

        let mut env = Environment::new();
        minijinja_embed::load_templates!(&mut env);

        env.add_function(
            "_",
            move |msg_id: &str, kwargs: minijinja::value::Kwargs, state: &minijinja::State| {
                translate(&localizer, msg_id, kwargs, state)
            },
        );

        for plugin in self.plugins.iter() {
            plugin.theme(&mut env);
        }

        let router_state = Arc::new(RhombusRouterStateInner {
            db: self.db.clone(),
            jinja: env,
            config: self.config.clone(),
            discord_signin_url: format!(
                "https://discord.com/api/oauth2/authorize?client_id={}&redirect_uri={}&response_type=code&scope=identify+guilds.join",
                self.config.discord_client_id,
                format!("{}/signin/discord", self.config.location_url)
            ),
        });

        let mut plugin_router = Router::new();
        for plugin in self.plugins.iter() {
            plugin_router = plugin_router.merge(plugin.routes(router_state.clone()));
        }

        let rhombus_router = Router::new()
            .fallback(handler_404)
            .route("/account", get(route_account))
            .route_layer(middleware::from_fn(enforce_auth_middleware))
            .nest_service("/static", ServeDir::new(STATIC_DIR))
            .route("/", get(route_home))
            .route("/challenges", get(route_challenges))
            .route("/modal", get(route_command_palette))
            .route("/signout", get(route_signout))
            .route("/signin", get(route_signin))
            .route("/signin/discord", get(route_discord_callback))
            .route_layer(middleware::from_fn(locales::locale))
            .route_layer(middleware::from_fn_with_state(router_state.clone(), track))
            .route_layer(middleware::from_fn_with_state(
                router_state.clone(),
                auth_injector_middleware,
            ))
            .route("/og-image.png", get(route_default_og_image))
            .with_state(router_state.clone());

        let router = if self.plugins.is_empty() {
            rhombus_router
        } else {
            Router::new()
                .fallback_service(rhombus_router)
                .nest("/", plugin_router)
        };

        let governor_conf = Box::new(
            GovernorConfigBuilder::default()
                .per_second(1)
                .burst_size(50)
                .use_headers()
                .finish()
                .unwrap(),
        );

        let router = router
            .route_layer(middleware::from_fn(locales::locale))
            .route_layer(middleware::from_fn_with_state(router_state.clone(), track))
            .route_layer(middleware::from_fn_with_state(
                router_state.clone(),
                auth_injector_middleware,
            ))
            .layer(GovernorLayer {
                config: Box::leak(governor_conf),
            });

        #[cfg(debug_assertions)]
        let router = router
            .layer(tower_livereload::LiveReloadLayer::new().request_predicate(not_htmx_predicate));

        let router = router.layer(CompressionLayer::new());

        router
    }
}

#[cfg(debug_assertions)]
fn not_htmx_predicate<T>(req: &axum::http::Request<T>) -> bool {
    !req.headers().contains_key("hx-request")
}

pub async fn serve(router: Router, address: impl ToSocketAddrs) -> Result<(), std::io::Error> {
    #[cfg(debug_assertions)]
    let listener = match listenfd::ListenFd::from_env().take_tcp_listener(0).unwrap() {
        Some(listener) => {
            tracing::debug!("restored socket from listenfd");
            listener.set_nonblocking(true).unwrap();
            TcpListener::from_std(listener).unwrap()
        }
        None => TcpListener::bind(address).await.unwrap(),
    };

    #[cfg(not(debug_assertions))]
    let listener = TcpListener::bind(address).await.unwrap();

    let address = listener.local_addr().unwrap().to_string();
    info!(address, "listening on {}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

async fn route_home(
    State(state): State<RhombusRouterState>,
    Extension(user): Extension<MaybeClientUser>,
    Extension(lang): Extension<Lang>,
    uri: Uri,
) -> Html<String> {
    Html(
        state
            .jinja
            .get_template("home.html")
            .unwrap()
            .render(context! {
                lang => lang,
                user => user,
                uri => uri.to_string(),
                location_url => state.config.location_url,
                discord_signin_url => &state.discord_signin_url,
                og_image => format!("{}/og-image.png", state.config.location_url)
            })
            .unwrap(),
    )
}
