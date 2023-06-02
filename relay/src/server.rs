use actix::prelude::*;
use actix_cors::Cors;
use actix_http::header::{ACCEPT, UPGRADE};
use actix_web::{
    body::MessageBody,
    dev::{ServiceFactory, ServiceRequest},
    web, App, Error, HttpRequest, HttpResponse, HttpServer,
};
use actix_web_actors::ws;
use std::net::{SocketAddr, ToSocketAddrs};

pub struct UserWebSocket {
    addr: SocketAddr,
}

impl Actor for UserWebSocket {
    type Context = ws::WebsocketContext<Self>;

    /// Method is called on actor start. We start the heartbeat process here.
    fn started(&mut self, _ctx: &mut Self::Context) {}

    fn stopped(&mut self, _ctx: &mut Self::Context) {}
}

/// Handler for `ws::Message`
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for UserWebSocket {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        // debug!("WEBSOCKET MESSAGE: {msg:?}");
        let msg = match msg {
            Err(_err) => {
                ctx.stop();
                return;
            }
            Ok(msg) => msg,
        };
        match msg {
            ws::Message::Text(_text) => {}
            ws::Message::Ping(msg) => ctx.pong(&msg),
            ws::Message::Close(reason) => {
                ctx.close(reason);
                ctx.stop();
            }
            _ => {}
        }
    }
}

/// WebSocket handshake and start `UserWebSocket` actor.
async fn index(req: HttpRequest, stream: web::Payload) -> Result<HttpResponse, Error> {
    let headers = req.headers();
    if headers.contains_key(UPGRADE) {
        return ws::start(
            UserWebSocket {
                addr: req.peer_addr().unwrap(),
            },
            &req,
            stream,
        );
    } else if let Some(accept) = headers.get(ACCEPT) {
        let t = "application/nostr+json";
        if let Ok(accept) = accept.to_str() {
            if accept.contains(t) {
                return Ok(HttpResponse::Ok()
                    .insert_header(("Content-Type", t))
                    .body("info"));
            }
        }
    }

    Ok(HttpResponse::Ok().body("Hello World!"))
}

pub fn create_app() -> App<
    impl ServiceFactory<
        ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse<impl MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    let app = App::new();
    app.service(web::resource("/").route(web::get().to(index)))
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
    HttpServer::new(|| create_app())
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
    use anyhow::Result;
    use bytes::Bytes;
    use futures_util::{SinkExt as _, StreamExt as _};

    #[actix_rt::test]
    async fn relay_info() -> Result<()> {
        let app = init_service(create_app()).await;
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
        let mut srv = actix_test::start(|| create_app());

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
