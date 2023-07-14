use metrics::{describe_histogram, histogram};
use nostr_relay::{
    db::{Db, Filter},
    duration::NonZeroDuration,
    message::{ClientMessage, IncomingMessage, OutgoingMessage},
    setting::SettingWrapper,
    Error, Extension, ExtensionMessageResult, Session,
};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;

#[derive(Deserialize, Default, Debug)]
pub struct CountSetting {
    pub enabled: bool,
}

pub struct Count {
    setting: CountSetting,
    db: Arc<Db>,
}

impl Count {
    pub fn new(db: Arc<Db>) -> Self {
        describe_histogram!("nostr_relay_count_size", "The time of per filter count");
        Self {
            setting: CountSetting::default(),
            db,
        }
    }

    fn count(&self, filter: &Filter, timeout: Option<NonZeroDuration>) -> Result<u64, Error> {
        let reader = self.db.reader()?;
        let start = Instant::now();
        let mut iter = self.db.iter::<String, _>(&reader, filter)?;
        if let Some(time) = timeout {
            iter.scan_time(time.into(), 2000);
        }
        let (size, _) = iter.size()?;
        histogram!("nostr_relay_count_size", start.elapsed());
        Ok(size)
    }
}

impl Extension for Count {
    fn name(&self) -> &'static str {
        "count"
    }

    fn setting(&mut self, setting: &SettingWrapper) {
        let mut w = setting.write();
        self.setting = w.parse_extension(self.name());
        if self.setting.enabled {
            w.add_nip(45);
        }
    }

    fn message(
        &self,
        msg: ClientMessage,
        session: &mut Session,
        _ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        if self.setting.enabled {
            if let IncomingMessage::Count(sub) = &msg.msg {
                if !sub.filters.is_empty() {
                    let timeout = session.app.setting.read().data.db_query_timeout;
                    match self.count(&sub.filters[0], timeout) {
                        Ok(size) => {
                            return ExtensionMessageResult::Stop(OutgoingMessage(format!(
                                r#"["COUNT","{}",{{"count": {}}}]"#,
                                sub.id, size
                            )))
                        }
                        Err(err) => {
                            return ExtensionMessageResult::Stop(OutgoingMessage::notice(&format!(
                                "count event error: {}",
                                err
                            )))
                        }
                    }
                }
            }
        }
        ExtensionMessageResult::Continue(msg)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::create_test_app;
    use actix_web::web;
    use actix_web_actors::ws;
    use anyhow::Result;
    use futures_util::{SinkExt as _, StreamExt as _};
    use nostr_relay::create_web_app;
    use nostr_relay::db::{
        now,
        secp256k1::{rand::thread_rng, KeyPair},
        Event,
    };

    fn parse_text<T: serde::de::DeserializeOwned>(frame: &ws::Frame) -> Result<T> {
        if let ws::Frame::Text(text) = &frame {
            // println!("message: {:?}", String::from_utf8(text.to_vec()));
            let data: T = serde_json::from_slice(text)?;
            Ok(data)
        } else {
            Err(nostr_relay::Error::Message("invalid frame type".to_string()).into())
        }
    }

    #[derive(Deserialize, Default, Debug)]
    struct CountResult {
        pub count: u64,
    }

    #[actix_rt::test]
    async fn message() -> Result<()> {
        let mut rng = thread_rng();
        let key_pair = KeyPair::new_global(&mut rng);

        let app = create_test_app("count")?;
        {
            let mut w = app.setting.write();
            w.extra = serde_json::from_str(
                r#"{
                "count": {
                    "enabled": true
                }
            }"#,
            )?;
        }
        let db = app.db.clone();
        let app = app.add_extension(Count::new(db));
        let app = web::Data::new(app);

        let mut srv = actix_test::start(move || create_web_app(app.clone()));

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        let start = now();
        // write
        for index in 0..10 {
            let event = Event::create(
                &key_pair,
                start + index as u64,
                1000 + index % 3,
                vec![],
                "test".to_owned(),
            )?;
            let msg = format!(r#"["EVENT", {}]"#, event.to_string());
            framed.send(ws::Message::Text(msg.into())).await?;
            let notice: (String, String, bool, String) =
                parse_text(&framed.next().await.unwrap()?)?;
            assert!(notice.2);
        }

        // count
        framed
            .send(ws::Message::Text(r#"["COUNT", "1", {}]"#.into()))
            .await?;
        let res: (String, String, CountResult) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(res.2.count, 10);

        framed
            .send(ws::Message::Text(
                r#"["COUNT", "1", {"kinds": [1002]}]"#.into(),
            ))
            .await?;
        let res: (String, String, CountResult) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(res.2.count, 3);

        // close
        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));

        Ok(())
    }
}
