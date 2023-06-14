use crate::{setting::SettingWrapper, Extension, Extensions, Result, Server, Setting};
use actix::Addr;
use actix_cors::Cors;
use actix_web::{
    body::MessageBody,
    dev::{ServiceFactory, ServiceRequest},
    web, App as WebApp, HttpServer,
};
use metrics::describe_counter;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use nostr_db::Db;
use notify::RecommendedWatcher;
use parking_lot::RwLock;
use std::{path::Path, sync::Arc};
use tracing::info;

pub mod route {
    use crate::{App, Session};
    use actix_http::header::{ACCEPT, UPGRADE};
    use actix_web::{web, Error, HttpRequest, HttpResponse};
    use actix_web_actors::ws;

    pub async fn websocket(
        req: HttpRequest,
        stream: web::Payload,
        data: web::Data<App>,
    ) -> Result<HttpResponse, Error> {
        let r = data.setting.read();
        let ip = if let Some(header) = &r.network.real_ip_header {
            header.iter().find_map(|s| {
                let hdr = req.headers().get(s)?.to_str().ok()?;
                let val = hdr.split(',').next()?.trim();
                Some(val.to_string())
            })
        } else {
            req.peer_addr().map(|a| a.ip().to_string())
        };
        drop(r);
        ws::start(Session::new(ip.unwrap_or_default(), data), &req, stream)
    }

    pub async fn information(
        _req: HttpRequest,
        _stream: web::Payload,
        data: web::Data<App>,
    ) -> Result<HttpResponse, Error> {
        let r = data.setting.read();
        Ok(HttpResponse::Ok()
            .insert_header(("Content-Type", "application/nostr+json"))
            .body(r.render_information()?))
    }

    pub async fn index(
        req: HttpRequest,
        stream: web::Payload,
        data: web::Data<App>,
    ) -> Result<HttpResponse, Error> {
        let headers = req.headers();
        if headers.contains_key(UPGRADE) {
            return websocket(req, stream, data).await;
        } else if let Some(accept) = headers.get(ACCEPT) {
            if let Ok(accept) = accept.to_str() {
                if accept.contains("application/nostr+json") {
                    return information(req, stream, data).await;
                }
            }
        }

        Ok(HttpResponse::Ok().body("Hello World!"))
    }

    pub async fn metrics(
        _req: HttpRequest,
        _stream: web::Payload,
        data: web::Data<App>,
    ) -> Result<HttpResponse, Error> {
        Ok(HttpResponse::Ok()
            .insert_header(("Content-Type", "text/plain"))
            .body(data.prometheus_handle.render()))
    }
}

/// App with data
pub struct App {
    pub server: Addr<Server>,
    pub db: Arc<Db>,
    pub setting: SettingWrapper,
    pub prometheus_handle: PrometheusHandle,
    pub extensions: Arc<RwLock<Extensions>>,
    pub watcher: Option<RecommendedWatcher>,
}

impl App {
    /// db_path: overwrite setting db path
    pub fn create<P: AsRef<Path>>(
        setting_path: Option<P>,
        watch: bool,
        db_path: Option<P>,
        prometheus_handle: PrometheusHandle,
    ) -> Result<Self> {
        let extensions = Arc::new(RwLock::new(Extensions::default()));
        let c_extensions = Arc::clone(&extensions);
        let (setting, watcher) = if watch && setting_path.is_some() {
            let path = setting_path.as_ref().unwrap().as_ref();
            info!("Watch config {:?}", path);
            let r = Setting::watch(path, move |s| {
                let r = c_extensions.read();
                r.call_setting(s);
            })?;
            (r.0, Some(r.1))
        } else {
            if let Some(path) = setting_path {
                info!("Load config {:?}", path.as_ref());
                (Setting::read_wrapper(path.as_ref())?, None)
            } else {
                info!("Load default config");
                (Setting::default_wrapper(), None)
            }
        };

        let r = setting.read();
        let path = db_path
            .map(|p| p.as_ref().to_path_buf())
            .unwrap_or(r.db.path.clone())
            .join("events");
        drop(r);
        let db = Arc::new(Db::open(path)?);

        let server = Server::create_with(db.clone(), Arc::clone(&setting));

        Ok(Self {
            server,
            setting,
            db,
            prometheus_handle,
            extensions,
            watcher,
        })
    }

    pub fn add_extension<E: Extension + 'static>(self, ext: E) -> Self {
        ext.setting(&self.setting);
        {
            let mut w = self.extensions.write();
            w.add(ext);
        }
        self
    }

    pub fn web_app(
        self,
    ) -> WebApp<
        impl ServiceFactory<
            ServiceRequest,
            Config = (),
            Response = actix_web::dev::ServiceResponse<impl MessageBody>,
            Error = actix_web::Error,
            InitError = (),
        >,
    > {
        create_app(web::Data::new(self))
    }

    pub fn web_server(self) -> Result<actix_server::Server, std::io::Error> {
        describe_metrics();

        let r = self.setting.read();
        let num = if r.thread.reader == 0 {
            num_cpus::get()
        } else {
            r.thread.reader
        };
        let host = r.network.host.clone();
        let port = r.network.port;
        drop(r);
        info!("Start http server {}:{}", host, port);
        let data = web::Data::new(self);
        Ok(HttpServer::new(move || create_app(data.clone()))
            .workers(num)
            .bind((host, port))?
            .run())
    }
}

pub fn create_prometheus_handle() -> PrometheusHandle {
    let builder = PrometheusBuilder::new();
    builder
        // .idle_timeout(
        //     metrics_util::MetricKindMask::ALL,
        //     Some(std::time::Duration::from_secs(10)),
        // )
        .install_recorder()
        .unwrap()
}

fn create_app(
    data: web::Data<App>,
) -> WebApp<
    impl ServiceFactory<
        ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse<impl MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    let app = WebApp::new();
    let extensions = data.extensions.clone();
    app.app_data(data)
        .configure(|cfg| {
            extensions.read().call_config_web(cfg);
        })
        .service(web::resource("/metrics").route(web::get().to(route::metrics)))
        .service(web::resource("/").route(web::get().to(route::index)))
        .wrap(
            Cors::default()
                .send_wildcard()
                .allow_any_header()
                .allow_any_origin()
                .allow_any_method()
                .max_age(86_400), // 24h
        )
}

pub fn describe_metrics() {
    describe_counter!("new_connections", "The total count of new connections");
    describe_counter!("current_connections", "The number of current connections");
}

#[cfg(test)]
pub mod tests {
    use std::time::Duration;

    use crate::create_test_app;
    use actix_rt::time::sleep;
    use actix_web::{
        dev::Service,
        test::{init_service, read_body, TestRequest},
    };
    use actix_web_actors::ws;
    use anyhow::Result;
    use bytes::Bytes;
    use futures_util::{SinkExt as _, StreamExt as _};

    #[actix_rt::test]
    async fn relay_info() -> Result<()> {
        let data = create_test_app("")?;
        let app = init_service(data.web_app()).await;
        sleep(Duration::from_millis(50)).await;
        let req = TestRequest::with_uri("/")
            .insert_header(("Accept", "application/nostr+json"))
            .to_request();
        let res = app.call(req).await.unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.headers().get(actix_http::header::CONTENT_TYPE).unwrap(),
            "application/nostr+json"
        );

        Ok(())
    }

    #[actix_rt::test]
    async fn metrics() -> Result<()> {
        let data = create_test_app("")?;
        metrics::increment_counter!("test_metric");

        let app = init_service(data.web_app()).await;
        sleep(Duration::from_millis(50)).await;
        let req = TestRequest::with_uri("/metrics").to_request();
        let res = app.call(req).await.unwrap();
        assert_eq!(res.status(), 200);

        let result = read_body(res).await;
        let result = String::from_utf8(result.to_vec())?;
        assert!(result.contains("test_metric"));
        Ok(())
    }

    #[actix_rt::test]
    async fn connect_ws() -> Result<()> {
        let mut srv = actix_test::start(|| {
            let data = create_test_app("").unwrap();
            data.web_app()
        });

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        framed.send(ws::Message::Ping("text".into())).await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Pong(Bytes::copy_from_slice(b"text")));

        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));
        Ok(())
    }
}
