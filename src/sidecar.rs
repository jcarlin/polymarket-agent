use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::config::Config;

#[derive(Debug, Deserialize)]
pub struct SidecarHealth {
    pub status: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub trading_mode: Option<String>,
}

pub struct SidecarProcess {
    child: Option<Child>,
    health_url: String,
    client: Client,
    startup_timeout: Duration,
    health_interval: Duration,
}

impl SidecarProcess {
    pub async fn spawn(config: &Config) -> Result<Self> {
        let health_url = format!("{}/health", config.sidecar_url());
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .context("Failed to build sidecar HTTP client")?;

        info!(
            "Spawning Python sidecar on {}:{}",
            config.sidecar_host, config.sidecar_port
        );

        let child = Command::new("python3")
            .arg("sidecar/server.py")
            .env("SIDECAR_PORT", config.sidecar_port.to_string())
            .env("TRADING_MODE", config.trading_mode.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn Python sidecar process")?;

        let mut process = SidecarProcess {
            child: Some(child),
            health_url,
            client,
            startup_timeout: Duration::from_secs(config.sidecar_startup_timeout_secs),
            health_interval: Duration::from_millis(config.sidecar_health_interval_ms),
        };

        process.wait_for_healthy().await?;
        Ok(process)
    }

    async fn wait_for_healthy(&mut self) -> Result<()> {
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > self.startup_timeout {
                anyhow::bail!(
                    "Sidecar failed to become healthy within {}s",
                    self.startup_timeout.as_secs()
                );
            }

            // Check if child process has exited
            if let Some(ref mut child) = self.child {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        anyhow::bail!(
                            "Sidecar process exited during startup with status: {}",
                            status
                        );
                    }
                    Ok(None) => {} // still running
                    Err(e) => {
                        warn!("Failed to check sidecar status: {}", e);
                    }
                }
            }

            match self.health_check().await {
                Ok(health) => {
                    info!(
                        "Sidecar healthy: status={}, version={:?}",
                        health.status, health.version
                    );
                    return Ok(());
                }
                Err(e) => {
                    debug!("Sidecar not ready yet: {}", e);
                }
            }

            tokio::time::sleep(self.health_interval).await;
        }
    }

    pub async fn health_check(&self) -> Result<SidecarHealth> {
        let response = self
            .client
            .get(&self.health_url)
            .send()
            .await
            .context("Failed to reach sidecar health endpoint")?;

        let health: SidecarHealth = response
            .json()
            .await
            .context("Failed to parse sidecar health response")?;

        Ok(health)
    }

    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(_)) => false, // exited
                Ok(None) => true,     // still running
                Err(_) => false,
            },
            None => false,
        }
    }

    pub fn shutdown(&mut self) {
        if let Some(ref mut child) = self.child {
            info!("Shutting down sidecar process");
            match child.kill() {
                Ok(()) => {
                    let _ = child.wait();
                    info!("Sidecar process terminated");
                }
                Err(e) => {
                    error!("Failed to kill sidecar process: {}", e);
                }
            }
        }
        self.child = None;
    }
}

impl Drop for SidecarProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_health_check_success() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "version": "0.1.0",
                "trading_mode": "paper"
            })))
            .mount(&server)
            .await;

        let process = SidecarProcess {
            child: None,
            health_url: format!("{}/health", server.uri()),
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            startup_timeout: Duration::from_secs(5),
            health_interval: Duration::from_millis(100),
        };

        let health = process.health_check().await.unwrap();
        assert_eq!(health.status, "ok");
        assert_eq!(health.version, Some("0.1.0".to_string()));
        assert_eq!(health.trading_mode, Some("paper".to_string()));
    }

    #[tokio::test]
    async fn test_health_check_failure() {
        // No server running - should fail
        let process = SidecarProcess {
            child: None,
            health_url: "http://127.0.0.1:19999/health".to_string(),
            client: Client::builder()
                .timeout(Duration::from_secs(1))
                .build()
                .unwrap(),
            startup_timeout: Duration::from_secs(5),
            health_interval: Duration::from_millis(100),
        };

        assert!(process.health_check().await.is_err());
    }

    #[tokio::test]
    async fn test_sidecar_health_response_parsing() {
        // Test that SidecarHealth deserializes correctly with missing optional fields
        let json = serde_json::json!({"status": "ok"});
        let health: SidecarHealth = serde_json::from_value(json).unwrap();
        assert_eq!(health.status, "ok");
        assert!(health.version.is_none());
        assert!(health.trading_mode.is_none());
    }
}
