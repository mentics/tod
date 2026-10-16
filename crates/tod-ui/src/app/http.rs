//! The `HttpClient` gpui loads remote images through (markdown `![](https://…)`,
//! `img(url)`). Without one every such load fails with "No HttpClient available".
//!
//! Requests run on reqwest over one shared tokio runtime (reqwest needs a tokio
//! reactor; gpui's executor has none). A failure is returned as an error.

use std::sync::OnceLock;
use std::time::Duration;

use futures_util::future::BoxFuture;
use http_client::http::{HeaderValue, Request, Response};
use http_client::{AsyncBody, HttpClient, Url};

fn runtime() -> anyhow::Result<&'static tokio::runtime::Runtime> {
    static RT: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("tod-http")
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| anyhow::anyhow!("starting the HTTP runtime: {e}"))
}

pub(crate) struct ReqwestHttpClient {
    client: reqwest::Client,
    user_agent: HeaderValue,
}

impl ReqwestHttpClient {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let user_agent = HeaderValue::from_static(concat!("tod/", env!("CARGO_PKG_VERSION")));
        // Built inside the runtime: reqwest's builder may need its reactor.
        let _guard = runtime()?.enter();
        let client = reqwest::Client::builder()
            .user_agent(user_agent.clone())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow::anyhow!("building the HTTP client: {e}"))?;
        Ok(Self { client, user_agent })
    }
}

impl HttpClient for ReqwestHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(&self, req: Request<AsyncBody>) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let client = self.client.clone();
        let method = req.method().clone();
        let uri = req.uri().to_string();
        let headers = req.headers().clone();
        Box::pin(async move {
            let handle = runtime()?.spawn(async move {
                let resp = client
                    .request(method, &uri)
                    .headers(headers)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("{uri}: {e}"))?;
                let status = resp.status();
                let resp_headers = resp.headers().clone();
                let bytes = resp.bytes().await.map_err(|e| anyhow::anyhow!("reading {uri}: {e}"))?;
                let mut out = Response::builder().status(status);
                if let Some(h) = out.headers_mut() {
                    *h = resp_headers;
                }
                Ok::<_, anyhow::Error>(out.body(AsyncBody::from(bytes.to_vec()))?)
            });
            handle.await.map_err(|e| anyhow::anyhow!("HTTP task failed: {e}"))?
        })
    }
}
