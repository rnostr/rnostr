use crate::Server;
use actix::{Actor, Addr};
use actix_cors::Cors;
use actix_web::{
    body::MessageBody,
    dev::{ServiceFactory, ServiceRequest},
    web, App, HttpServer,
};
use std::net::ToSocketAddrs;

pub mod route {
    use crate::{Server, Session};
    use actix::Addr;
    use actix_http::header::{ACCEPT, UPGRADE};
    use actix_web::{web, Error, HttpRequest, HttpResponse};
    use actix_web_actors::ws;
    use std::time::Instant;

    pub async fn websocket(
        req: HttpRequest,
        stream: web::Payload,
        server: web::Data<Addr<Server>>,
    ) -> Result<HttpResponse, Error> {
        ws::start(
            Session {
                id: 0,
                ip: req
                    .connection_info()
                    .realip_remote_addr()
                    .map(ToOwned::to_owned)
                    .unwrap_or_default(),
                hb: Instant::now(),
                server: server.get_ref().clone(),
            },
            &req,
            stream,
        )
    }

    pub async fn info(_req: HttpRequest, _stream: web::Payload) -> Result<HttpResponse, Error> {
        Ok(HttpResponse::Ok()
            .insert_header(("Content-Type", "application/nostr+json"))
            .body("info"))
    }

    pub async fn index(
        req: HttpRequest,
        stream: web::Payload,
        server: web::Data<Addr<Server>>,
    ) -> Result<HttpResponse, Error> {
        let headers = req.headers();
        if headers.contains_key(UPGRADE) {
            return websocket(req, stream, server).await;
        } else if let Some(accept) = headers.get(ACCEPT) {
            if let Ok(accept) = accept.to_str() {
                if accept.contains("application/nostr+json") {
                    return info(req, stream).await;
                }
            }
        }

        Ok(HttpResponse::Ok().body("Hello World!"))
    }
}

pub fn create_app(
    server: Addr<Server>,
) -> App<
    impl ServiceFactory<
        ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse<impl MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    let app = App::new();
    app.app_data(web::Data::new(server))
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

pub async fn start_app<A: ToSocketAddrs>(addrs: A) -> Result<(), std::io::Error> {
    // start server actor
    let server = Server::default().start();
    HttpServer::new(move || create_app(server.clone()))
        // .workers(2)
        .bind(addrs)?
        .run()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let server = Server::default().start();
        let app = init_service(create_app(server)).await;
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
    async fn connect() -> Result<()> {
        let server = Server::default().start();

        let mut srv = actix_test::start(move || create_app(server.clone()));

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
