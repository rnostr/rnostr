use nostr_relay::{
    message::{ClientMessage, IncomingMessage},
    setting::SettingWrapper,
    Extension, ExtensionMessageResult, Session,
};
use serde::Deserialize;

#[derive(Deserialize, Default, Debug)]
pub struct SearchSetting {
    pub enabled: bool,
}

#[derive(Default, Debug)]
pub struct Search {
    setting: SearchSetting,
}

impl Search {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Extension for Search {
    fn name(&self) -> &'static str {
        "search"
    }

    fn setting(&mut self, setting: &SettingWrapper) {
        let mut w = setting.write();
        self.setting = w.parse_extension(self.name());
        if self.setting.enabled {
            w.add_nip(50);
        }
    }

    fn message(
        &self,
        mut msg: ClientMessage,
        _session: &mut Session,
        _ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        if self.setting.enabled {
            match &mut msg.msg {
                IncomingMessage::Event(event) => {
                    event.build_note_words();
                }
                IncomingMessage::Req(sub) => {
                    for filter in &mut sub.filters {
                        filter.build_words();
                    }
                }
                _ => {}
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

    #[actix_rt::test]
    async fn message() -> Result<()> {
        let mut rng = thread_rng();
        let key_pair = KeyPair::new_global(&mut rng);

        let app = create_test_app("search")?;
        {
            let mut w = app.setting.write();
            w.extra = serde_json::from_str(
                r#"{
                "search": {
                    "enabled": true
                }
            }"#,
            )?;
        }

        let app = app.add_extension(Search::new());
        let app = web::Data::new(app);

        let mut srv = actix_test::start(move || create_web_app(app.clone()));

        // client service
        let mut framed = srv.ws_at("/").await.unwrap();

        let start = now();
        // write
        for (index, content) in ["test", "来自中国的nostr用户", "nostr users from China"]
            .into_iter()
            .enumerate()
        {
            let event = Event::create(
                &key_pair,
                start + index as u64,
                1,
                vec![],
                content.to_owned(),
            )?;
            let msg = format!(r#"["EVENT", {}]"#, event.to_string());
            framed.send(ws::Message::Text(msg.into())).await?;
            let notice: (String, String, bool, String) =
                parse_text(&framed.next().await.unwrap()?)?;
            assert!(notice.2);
        }

        // get
        framed
            .send(ws::Message::Text(
                r#"["REQ", "1", {"search": "nostr"}]"#.into(),
            ))
            .await?;
        let res: (String, String, Event) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(res.2.content(), "来自中国的nostr用户");
        let res: (String, String, Event) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(res.2.content(), "nostr users from China");
        let res: (String, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(res.0, "EOSE");

        framed
            .send(ws::Message::Text(
                r#"["REQ", "2", {"search": "中国nostr"}]"#.into(),
            ))
            .await?;
        let res: (String, String, Event) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(res.2.content(), "来自中国的nostr用户");
        let res: (String, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(res.0, "EOSE");

        framed
            .send(ws::Message::Text(
                r#"["REQ", "3", {"search": "china nostr"}]"#.into(),
            ))
            .await?;
        let res: (String, String, Event) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(res.2.content(), "nostr users from China");
        let res: (String, String) = parse_text(&framed.next().await.unwrap()?)?;
        assert_eq!(res.0, "EOSE");

        // close
        framed
            .send(ws::Message::Close(Some(ws::CloseCode::Normal.into())))
            .await?;
        let item = framed.next().await.unwrap()?;
        assert_eq!(item, ws::Frame::Close(Some(ws::CloseCode::Normal.into())));

        Ok(())
    }
}
