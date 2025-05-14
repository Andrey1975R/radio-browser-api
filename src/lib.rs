use std::sync::Arc;
use async_trait::async_trait;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

#[derive(Error, Debug)]
pub enum RadioBrowserError {
    #[error("HTTP request failed: {0}")]
    RequestError(#[from] reqwest::Error),

    #[error("API error: {0}")]
    ApiError(String),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RadioStation {
    pub name: String,
    pub url: String,
    pub tags: Option<String>,
    pub country: Option<String>,
    pub votes: Option<i32>,
}

#[async_trait]
pub trait Cache {
    async fn get(&self, key: &str) -> Option<Vec<RadioStation>>;
    async fn set(&self, key: String, value: Vec<RadioStation>);
}

pub struct MemoryCache {
    cache: Arc<Mutex<LruCache<String, Vec<RadioStation>>>>,
}

impl MemoryCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: Arc::new(Mutex::new(LruCache::new(capacity))),
        }
    }
}

#[async_trait]
impl Cache for MemoryCache {
    async fn get(&self, key: &str) -> Option<Vec<RadioStation>> {
        self.cache.lock().await.get(key).cloned()
    }

    async fn set(&self, key: String, value: Vec<RadioStation>) {
        self.cache.lock().await.put(key, value);
    }
}

pub struct RadioBrowserClient {
    base_url: String,
    client: reqwest::Client,
    cache: Arc<dyn Cache + Send + Sync>,
}

impl RadioBrowserClient {
    pub fn new() -> Self {
        Self {
            base_url: "https://de1.api.radio-browser.info".to_string(),
            client: reqwest::Client::new(),
            cache: Arc::new(MemoryCache::new(100)),
        }
    }

    pub fn with_base_url(mut self, base_url: &str) -> Self {
        self.base_url = base_url.to_string();
        self
    }

    pub fn with_cache(self, cache: Arc<dyn Cache + Send + Sync>) -> Self {
        Self { cache, ..self }
    }

    pub async fn search_by_tag(&self, tag: &str, limit: usize) -> Result<Vec<RadioStation>, RadioBrowserError> {
        let cache_key = format!("search:{}:{}", tag, limit);

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        let url = format!("{}/json/stations/search?tag={}&limit={}", self.base_url, tag, limit);
        let stations = self.fetch_stations(&url).await?;

        self.cache.set(cache_key, stations.clone()).await;
        Ok(stations)
    }

    async fn fetch_stations(&self, url: &str) -> Result<Vec<RadioStation>, RadioBrowserError> {
        let response = self.client.get(url).send().await?;

        if !response.status().is_success() {
            return Err(RadioBrowserError::ApiError(response.text().await?));
        }

        let stations = response.json().await?;
        Ok(stations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};
    use tokio_test::block_on;

    struct TestCache {
        data: std::sync::Mutex<Option<Vec<RadioStation>>>,
    }

    #[async_trait]
    impl Cache for TestCache {
        async fn get(&self, _key: &str) -> Option<Vec<RadioStation>> {
            self.data.lock().unwrap().clone()
        }

        async fn set(&self, _key: String, value: Vec<RadioStation>) {
            *self.data.lock().unwrap() = Some(value);
        }
    }

    #[tokio::test]
    async fn test_search_with_cache() {
        // Запускаем mock сервер
        let mock_server = MockServer::start().await;

        // Настраиваем мок-ответ
        Mock::given(method("GET"))
            .and(path("/json/stations/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![
                RadioStation {
                    name: "Test Station".to_string(),
                    url: "http://test.com".to_string(),
                    votes: Some(100),
                    tags: None,
                    country: None,
                }
            ]))
            .mount(&mock_server)
            .await;

        let test_cache = Arc::new(TestCache {
            data: std::sync::Mutex::new(None),
        });

        let client = RadioBrowserClient {
            base_url: mock_server.uri(),
            client: reqwest::Client::new(),
            cache: test_cache.clone(),
        };

        // Первый запрос - должен закешироваться
        let stations = client.search_by_tag("test", 1).await.unwrap();
        assert_eq!(stations[0].name, "Test Station");

        // Второй запрос - должен взять из кеша
        let cached = client.search_by_tag("test", 1).await.unwrap();
        assert_eq!(cached[0].name, "Test Station");
    }

    #[test] // Теперь это обычный тест, а не асинхронный
    fn test_api_error() {
        block_on(async {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/json/stations/search"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&mock_server)
                .await;

            let client = RadioBrowserClient {
                base_url: mock_server.uri(),
                client: reqwest::Client::new(),
                cache: Arc::new(MemoryCache::new(10)),
            };

            let result = client.search_by_tag("test", 1).await;
            assert!(matches!(result, Err(RadioBrowserError::ApiError(_))));
        });
    }
}