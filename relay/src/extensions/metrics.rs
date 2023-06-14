use crate::Extension;
use actix_web::{web, HttpResponse};
use metrics::describe_counter;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

#[derive(Debug)]
pub struct Metrics {
    pub enabled: bool,
}

impl Metrics {
    pub fn new() -> Self {
        describe_metrics();
        Self { enabled: true }
    }
}

impl Extension for Metrics {
    fn name(&self) -> &'static str {
        "metrics"
    }

    fn config_web(&mut self, cfg: &mut actix_web::web::ServiceConfig) {
        let data = web::Data::new(create_prometheus_handle());
        cfg.app_data(data)
            .service(web::resource("/metrics").route(web::get().to(route_metrics)));
    }
}

pub fn describe_metrics() {
    describe_counter!("new_connections", "The total count of new connections");
    describe_counter!("current_connections", "The number of current connections");
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

pub async fn route_metrics(
    data: web::Data<PrometheusHandle>,
) -> Result<HttpResponse, actix_web::Error> {
    Ok(HttpResponse::Ok()
        .insert_header(("Content-Type", "text/plain"))
        .body(data.render()))
}

#[cfg(test)]
pub mod tests {
    use super::Metrics;
    use crate::create_test_app;
    use actix_rt::time::sleep;
    use actix_web::{
        dev::Service,
        test::{init_service, read_body, TestRequest},
    };
    use anyhow::Result;
    use std::time::Duration;

    #[actix_rt::test]
    async fn metrics() -> Result<()> {
        let data = create_test_app("")?.add_extension(Metrics::new());

        let app = init_service(data.web_app()).await;
        sleep(Duration::from_millis(50)).await;
        metrics::increment_counter!("test_metric");

        let req = TestRequest::with_uri("/metrics").to_request();
        let res = app.call(req).await.unwrap();
        assert_eq!(res.status(), 200);

        let result = read_body(res).await;
        let result = String::from_utf8(result.to_vec())?;
        assert!(result.contains("test_metric"));
        Ok(())
    }
}
