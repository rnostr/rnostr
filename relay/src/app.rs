use crate::{setting::SettingWrapper, Extension, Extensions, Result, Server, Setting};
use actix::Addr;
use actix_cors::Cors;
use actix_web::{
    body::MessageBody,
    dev::{ServiceFactory, ServiceRequest},
    web, App as WebApp, HttpServer,
};
use nostr_db::Db;
use parking_lot::RwLock;
use std::{path::Path, sync::Arc};
use tracing::info;

pub mod route {
    use crate::{App, Session};
    use actix_web::http::header::{ACCEPT, LOCATION, UPGRADE};
    use actix_web::{web, Error, HttpRequest, HttpResponse};
    use actix_web_actors::ws;

    fn get_ip(req: &HttpRequest, header: Option<&String>) -> Option<String> {
        if let Some(header) = header {
            // find from header list
            // header.iter().find_map(|s| {
            //     let hdr = req.headers().get(s)?.to_str().ok()?;
            //     let val = hdr.split(',').next()?.trim();
            //     Some(val.to_string())
            // })
            Some(
                req.headers()
                    .get(header)?
                    .to_str()
                    .ok()?
                    .split(',')
                    .next()?
                    .trim()
                    .to_string(),
            )
        } else {
            Some(req.peer_addr()?.ip().to_string())
        }
    }

    pub async fn websocket(
        req: HttpRequest,
        stream: web::Payload,
        data: web::Data<App>,
    ) -> Result<HttpResponse, Error> {
        let r = data.setting.read();
        let ip = get_ip(&req, r.network.real_ip_header.as_ref());
        let max_size = r.limitation.max_message_length;
        drop(r);

        let session = Session::new(ip.unwrap_or_default(), data);

        // ws::start(session, &req, stream)
        // The default max frame size is 60k, change from setting.
        ws::WsResponseBuilder::new(session, &req, stream)
            .frame_size(max_size)
            .start()
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

        let s = data.setting.read();
        if let Some(site) = &s.network.index_redirect_to {
            Ok(HttpResponse::Found()
                .append_header((LOCATION, site.as_str()))
                .finish())
        } else {
            Ok(HttpResponse::Ok().body(s.information.description.clone()))
        }
    }
}

/// App with data
pub struct App {
    pub server: Addr<Server>,
    pub db: Arc<Db>,
    pub setting: SettingWrapper,
    pub extensions: Arc<RwLock<Extensions>>,
}

impl App {
    /// data_path: overwrite setting data path
    pub fn create<P: AsRef<Path>>(
        setting_path: Option<P>,
        watch: bool,
        setting_env_prefix: Option<String>,
        data_path: Option<P>,
    ) -> Result<Self> {
        let extensions = Arc::new(RwLock::new(Extensions::default()));
        let c_extensions = Arc::clone(&extensions);
        let env_notice = setting_env_prefix
            .as_ref()
            .map(|s| {
                format!(
                    ", config will be overrided by ENV seting with prefix `{}_`",
                    s
                )
            })
            .unwrap_or_default();

        let setting = if watch && setting_path.is_some() {
            let path = setting_path.as_ref().unwrap().as_ref();
            info!("Watch config file {:?}{}", path, env_notice);
            SettingWrapper::watch(path, setting_env_prefix, move |s| {
                let mut w = c_extensions.write();
                w.call_setting(s);
            })?
        } else if let Some(path) = setting_path {
            info!("Load config {:?}{}", path.as_ref(), env_notice);
            Setting::read(path.as_ref(), setting_env_prefix)?.into()
        } else if let Some(prefix) = setting_env_prefix {
            info!("Load default config{}", env_notice);
            Setting::from_env(prefix)?.into()
        } else {
            info!("Load default config");
            Setting::default().into()
        };

        {
            info!("{:?}", setting.read());
        }

        let r = setting.read();
        let path = data_path
            .map(|p| p.as_ref().to_path_buf())
            .unwrap_or_else(|| r.data.path.clone())
            .join("events");
        drop(r);
        let db = Arc::new(Db::open(path)?);
        db.check_schema()?;

        let server = Server::create_with(db.clone(), setting.clone());

        Ok(Self {
            server,
            setting,
            db,
            extensions,
        })
    }

    pub fn add_extension<E: Extension + 'static>(self, mut ext: E) -> Self {
        info!("Add extension {}", ext.name());
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
        create_web_app(web::Data::new(self))
    }

    pub fn web_server(self) -> Result<actix_web::dev::Server, std::io::Error> {
        let r = self.setting.read();
        let num = if r.thread.http == 0 {
            num_cpus::get()
        } else {
            r.thread.http
        };
        let host = r.network.host.clone();
        let port = r.network.port;
        drop(r);
        info!("Start http server {}:{}", host, port);
        let data = web::Data::new(self);
        Ok(HttpServer::new(move || create_web_app(data.clone()))
            .workers(num)
            .bind((host, port))?
            .run())
    }
}

pub fn create_web_app(
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
            extensions.write().call_config_web(cfg);
        })
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

#[cfg(test)]
pub mod tests {
    use std::time::Duration;

    use crate::create_test_app;
    use actix_rt::time::sleep;
    use actix_test::read_body;
    use actix_web::{
        dev::Service,
        test::{init_service, TestRequest},
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
            res.headers()
                .get(actix_web::http::header::CONTENT_TYPE)
                .unwrap(),
            "application/nostr+json"
        );
        let result = read_body(res).await;
        let result = String::from_utf8(result.to_vec())?;
        assert!(result.contains("supported_nips"));
        assert!(result.contains("limitation"));
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
