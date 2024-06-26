use confique::Config;
use dotenv::dotenv;
use once_cell::sync::OnceCell;
use salvo::{
    http::{header, HeaderValue},
    prelude::*,
};
use serde::{Deserialize, Serialize};
use sqlx::{migrate::MigrateDatabase, types::chrono::NaiveDateTime, FromRow, Sqlite, SqlitePool};
use std::sync::OnceLock;
use tokio::join;

static SQLITE: OnceCell<SqlitePool> = OnceCell::new();

#[derive(Config)]
struct Conf {
    #[config(env = "CORS_ALLOW_ORIGINS", default = "*")]
    cors_allow_origins: String,

    #[config(env = "FORWARD_URL", default = "https://example.com/")]
    forward_url: String,
}

fn config() -> &'static Conf {
    static CONFIG: OnceLock<Conf> = OnceLock::new();
    CONFIG.get_or_init(|| Conf::builder().env().load().unwrap())
}

fn sqlite() -> &'static SqlitePool {
    SQLITE.get().unwrap()
}

// Serialize, Deserialize
#[derive(FromRow, Debug)]
struct Link {
    slug: String,
    url: String,
    created_at: Option<NaiveDateTime>,
    updated_at: Option<NaiveDateTime>,
}

// Serialize, Deserialize
#[derive(FromRow, Debug)]
struct LinkClickTracking {
    slug: String,
    datetime: Option<NaiveDateTime>,
    client_ip_address: String,
    client_browser: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct LinkClickTrackingDTO {
    slug: String,
    datetime: String,
    client_ip_address: String,
    client_browser: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct LinkDTO {
    slug: String,
    url: String,
    url_slug: String,
    created_at: String,
    updated_at: String,
    tracking_clicks: i32,
}

#[derive(Serialize, Deserialize, Debug)]
struct CreateLinkDTO {
    slug: String,
    url: String,
}

#[handler]
async fn get_links(_req: &mut Request, res: &mut Response) {
    let data = sqlx::query!(
        "SELECT t1.*, COUNT(t2.datetime) as tracking_clicks FROM links t1
        LEFT JOIN link_click_tracking t2 ON t1.slug = t2.slug
        GROUP BY t1.slug"
    )
    .fetch_all(sqlite())
    .await;

    match data {
        Ok(data) => {
            let links = data
                .into_iter()
                .map(|record| LinkDTO {
                    url_slug: format!(
                        "{}/{}",
                        config().forward_url.trim_end_matches("/"),
                        record.slug
                    )
                    .to_string(),
                    created_at: record.created_at.unwrap_or_default().to_string(),
                    updated_at: record.updated_at.unwrap_or_default().to_string(),
                    slug: record.slug,
                    url: record.url,
                    tracking_clicks: record.tracking_clicks,
                })
                .collect::<Vec<LinkDTO>>();
            res.render(Json(links));
        }
        Err(err) => res
            .status_code(StatusCode::INTERNAL_SERVER_ERROR)
            .render(Json(err.to_string())),
    }
}

#[handler]
async fn get_link_clicks(req: &mut Request, res: &mut Response) {
    let slug = req
        .params()
        .get("slug")
        .unwrap_or(&"".to_string())
        .to_string();

    let data = sqlx::query_as!(
        LinkClickTracking,
        "SELECT * FROM link_click_tracking WHERE slug = ?",
        slug
    )
    .fetch_all(sqlite())
    .await;

    match data {
        Ok(data) => {
            res.render(Json(
                data.into_iter()
                    .map(|record| LinkClickTrackingDTO {
                        slug: record.slug,
                        client_browser: record.client_browser,
                        client_ip_address: record.client_ip_address,
                        datetime: record.datetime.unwrap().to_string(),
                    })
                    .collect::<Vec<LinkClickTrackingDTO>>(),
            ));
        }
        Err(err) => res
            .status_code(StatusCode::INTERNAL_SERVER_ERROR)
            .render(Json(err.to_string())),
    }
}

#[handler]
async fn create_link(req: &mut Request, res: &mut Response) {
    let link = match req.parse_json::<CreateLinkDTO>().await {
        Ok(link) => link,
        Err(err) => {
            return res
                .status_code(StatusCode::UNPROCESSABLE_ENTITY)
                .render(Json(err.to_string()));
        }
    };
    match sqlx::query!(
        "INSERT INTO links (slug, url) VALUES (?, ?)",
        link.slug,
        link.url
    )
    .execute(sqlite())
    .await
    {
        Ok(_) => res.status_code(StatusCode::CREATED),
        Err(err) => {
            return res
                .status_code(StatusCode::UNPROCESSABLE_ENTITY)
                .render(Json(err.to_string()))
        }
    };
}

#[handler]
async fn delete_link(req: &mut Request, res: &mut Response) {
    let slug = req
        .params()
        .get("slug")
        .unwrap_or(&"".to_string())
        .to_string();

    match sqlx::query!("DELETE FROM links WHERE slug = ?", slug)
        .execute(sqlite())
        .await
    {
        Ok(_) => res.status_code(StatusCode::NO_CONTENT),
        Err(err) => {
            return res
                .status_code(StatusCode::UNPROCESSABLE_ENTITY)
                .render(Json(err.to_string()));
        }
    };
}

fn get_user_ip(req: &mut Request) -> String {
    if !get_header(req, "X-Real-Ip").is_empty() {
        return get_header(req, "X-Real-Ip");
    }

    if !get_header(req, "X-Forwarded-For").is_empty() {
        return get_header(req, "X-Forwarded-For");
    }

    if !get_header(req, "RemoteAddr").is_empty() {
        return get_header(req, "RemoteAddr");
    }

    return "".to_string();
}

fn get_header(req: &Request, key: &str) -> String {
    req.headers()
        .get(key)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

#[handler]
async fn redirect_handler(req: &mut Request, res: &mut Response) {
    let slug = req.params().get("slug").cloned().unwrap_or_default();
    let data = sqlx::query_as!(Link, "SELECT * FROM links WHERE slug = ?", slug)
        .fetch_one(sqlite())
        .await;

    sqlx::query("INSERT INTO link_click_tracking (slug, client_ip_address, client_browser) VALUES (?, ?, ?)")
        .bind(&slug)
        .bind(get_user_ip(req))
        .bind(get_header(req, "User-Agent"))
        .execute(sqlite())
        .await
        .unwrap_or_default();

    match data {
        Ok(link) => {
            res.headers_mut().insert(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            );
            res.render(Redirect::found(link.url))
        }
        Err(_) => {
            res.status_code(StatusCode::NOT_FOUND)
                .render(Text::Plain("Not found."));
        }
    }
}

#[handler]
async fn cors(_req: &mut Request, res: &mut Response) {
    res.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static(config().cors_allow_origins.as_str()),
    );

    res.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );

    res.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("*"),
    );
}

#[handler]
async fn content_type(_req: &mut Request, res: &mut Response) {
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
}

#[handler]
fn ok_handler(_req: &mut Request, res: &mut Response) {
    res.status_code(StatusCode::NO_CONTENT);
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    tracing_subscriber::fmt().init();

    let db_url = "sqlite://db/links.db";
    if !Sqlite::database_exists(db_url).await.unwrap_or(false) {
        Sqlite::create_database(db_url).await.unwrap();
    }

    let pool = SqlitePool::connect(db_url).await.unwrap();

    match sqlx::migrate!().run(&pool).await {
        Ok(_) => println!("Sucessfully run migrations."),
        Err(e) => {
            eprintln!("Failed to run DB migrations: {:?}", e);
            return;
        }
    }

    SQLITE.set(pool).unwrap();

    let router_admin = Router::new()
        .hoop(cors)
        .hoop(content_type)
        .push(Router::with_path("status").get(ok_handler))
        .push(
            Router::with_path("links")
                .get(get_links)
                .post(create_link)
                .options(ok_handler)
                .push(
                    Router::with_path("<slug>")
                        .push(Router::with_path("/clicks").get(get_link_clicks))
                        .delete(delete_link)
                        .options(ok_handler),
                ),
        )
        .push(Router::with_path("icon.svg").get(StaticFile::new("static/icon.svg")))
        .push(
            Router::with_path("<**path>").get(
                StaticDir::new(["static"])
                    .defaults("index.html")
                    .auto_list(true),
            ),
        );
    let acceptor_admin = TcpListener::new("0.0.0.0:3000").bind().await;

    // Start a separate port for the forwarding service.
    let router_forwarder = Router::new().push(Router::with_path("<slug>").goal(redirect_handler));
    let acceptor_forwarder = TcpListener::new("0.0.0.0:3001").bind().await;

    // Start the servers
    join!(
        Server::new(acceptor_admin).serve(Service::new(router_admin).hoop(Logger::new())),
        Server::new(acceptor_forwarder).serve(Service::new(router_forwarder).hoop(Logger::new()))
    );
}
