//! Rate Limiting Module
//!
//! Provides token bucket rate limiting for API endpoints.
//! Prevents abuse and ensures fair resource usage.
//!
//! # Features
//!
//! - Token bucket algorithm
//! - Per-user and per-IP limiting
//! - Configurable limits per endpoint
//! - Distributed rate limiting support

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Rate limit configuration
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum tokens (burst capacity)
    pub max_tokens: u32,
    /// Tokens added per second
    pub refill_rate: f32,
    /// Window size for sliding window (seconds)
    pub window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_tokens: 100,
            refill_rate: 10.0,
            window_secs: 60,
        }
    }
}

/// Token bucket for rate limiting
pub struct TokenBucket {
    /// Maximum tokens
    max_tokens: u32,
    /// Current tokens
    tokens: f32,
    /// Last refill time
    last_refill: Instant,
    /// Refill rate (tokens per second)
    refill_rate: f32,
}

impl TokenBucket {
    pub fn new(max_tokens: u32, refill_rate: f32) -> Self {
        Self {
            max_tokens,
            tokens: max_tokens as f32,
            last_refill: Instant::now(),
            refill_rate,
        }
    }

    /// Try to consume a token
    pub fn try_consume(&mut self, tokens: u32) -> bool {
        self.refill();

        if self.tokens >= tokens as f32 {
            self.tokens -= tokens as f32;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f32();
        
        let new_tokens = elapsed * self.refill_rate;
        self.tokens = (self.tokens + new_tokens).min(self.max_tokens as f32);
        self.last_refill = now;
    }

    /// Get current token count
    pub fn tokens(&self) -> u32 {
        self.tokens as u32
    }

    /// Get time until next token
    pub fn time_until_token(&self) -> Duration {
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            let needed = 1.0 - self.tokens;
            let secs = needed / self.refill_rate;
            Duration::from_secs_f32(secs)
        }
    }
}

/// Rate limiter with per-key buckets
pub struct RateLimiter {
    /// Default configuration
    default_config: RateLimitConfig,
    /// Per-key buckets
    buckets: Arc<RwLock<HashMap<String, TokenBucket>>>,
    /// Per-endpoint configurations
    endpoint_configs: Arc<RwLock<HashMap<String, RateLimitConfig>>>,
}

impl RateLimiter {
    pub fn new(default_config: RateLimitConfig) -> Self {
        Self {
            default_config,
            buckets: Arc::new(RwLock::new(HashMap::new())),
            endpoint_configs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Set rate limit for a specific endpoint
    pub async fn set_endpoint_limit(&self, endpoint: &str, config: RateLimitConfig) {
        let mut configs = self.endpoint_configs.write().await;
        configs.insert(endpoint.to_string(), config);
    }

    /// Check if request is allowed
    pub async fn check(&self, key: &str, endpoint: Option<&str>) -> RateLimitResult {
        // Get config for endpoint or default
        let config = if let Some(ep) = endpoint {
            let configs = self.endpoint_configs.read().await;
            configs.get(ep).cloned().unwrap_or_else(|| self.default_config.clone())
        } else {
            self.default_config.clone()
        };

        // Get or create bucket
        let mut buckets = self.buckets.write().await;
        let bucket = buckets.entry(key.to_string())
            .or_insert_with(|| TokenBucket::new(config.max_tokens, config.refill_rate));

        // Try to consume token
        if bucket.try_consume(1) {
            RateLimitResult::Allowed
        } else {
            RateLimitResult::Limited {
                retry_after: bucket.time_until_token(),
                limit: config.max_tokens,
                remaining: bucket.tokens(),
            }
        }
    }

    /// Check multiple keys (for distributed limiting)
    pub async fn check_batch(&self, keys: &[&str]) -> Vec<RateLimitResult> {
        let mut results = Vec::with_capacity(keys.len());
        
        for key in keys {
            let result = self.check(key, None).await;
            results.push(result);
        }
        
        results
    }

    /// Get current limit status for a key
    pub async fn get_status(&self, key: &str) -> Option<LimitStatus> {
        let buckets = self.buckets.read().await;
        buckets.get(key).map(|bucket| {
            LimitStatus {
                limit: bucket.max_tokens,
                remaining: bucket.tokens(),
                reset: bucket.time_until_token(),
            }
        })
    }

    /// Reset limit for a key
    pub async fn reset(&self, key: &str) {
        let mut buckets = self.buckets.write().await;
        if let Some(bucket) = buckets.get_mut(key) {
            bucket.tokens = bucket.max_tokens as f32;
            bucket.last_refill = Instant::now();
        }
    }

    /// Remove limit for a key
    pub async fn remove(&self, key: &str) {
        let mut buckets = self.buckets.write().await;
        buckets.remove(key);
    }

    /// Cleanup old buckets
    pub async fn cleanup(&self, max_age: Duration) {
        let mut buckets = self.buckets.write().await;
        let now = Instant::now();
        
        buckets.retain(|_, bucket| {
            now.duration_since(bucket.last_refill) < max_age
        });
    }
}

/// Rate limit check result
#[derive(Debug, Clone)]
pub enum RateLimitResult {
    Allowed,
    Limited {
        retry_after: Duration,
        limit: u32,
        remaining: u32,
    },
}

impl RateLimitResult {
    pub fn is_allowed(&self) -> bool {
        matches!(self, RateLimitResult::Allowed)
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            RateLimitResult::Allowed => None,
            RateLimitResult::Limited { retry_after, .. } => Some(*retry_after),
        }
    }
}

/// Limit status information
#[derive(Debug, Clone)]
pub struct LimitStatus {
    pub limit: u32,
    pub remaining: u32,
    pub reset: Duration,
}

/// Sliding window rate limiter (alternative algorithm)
pub struct SlidingWindowLimiter {
    /// Window size in seconds
    window_secs: u64,
    /// Max requests per window
    max_requests: u32,
    /// Per-key request timestamps
    requests: Arc<RwLock<HashMap<String, VecDeque<Instant>>>>,
}

impl SlidingWindowLimiter {
    pub fn new(window_secs: u64, max_requests: u32) -> Self {
        use std::collections::VecDeque;
        
        Self {
            window_secs,
            max_requests,
            requests: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn check(&self, key: &str) -> RateLimitResult {
        use std::collections::VecDeque;
        
        let mut requests = self.requests.write().await;
        let now = Instant::now();
        let window_start = now - Duration::from_secs(self.window_secs);

        let timestamps = requests.entry(key.to_string())
            .or_insert_with(VecDeque::new);

        // Remove old timestamps
        while let Some(&ts) = timestamps.front() {
            if ts < window_start {
                timestamps.pop_front();
            } else {
                break;
            }
        }

        // Check if under limit
        if timestamps.len() < self.max_requests as usize {
            timestamps.push_back(now);
            RateLimitResult::Allowed
        } else {
            // Calculate retry time
            let oldest = timestamps.front().unwrap();
            let retry_after = oldest + Duration::from_secs(self.window_secs) - now;
            
            RateLimitResult::Limited {
                retry_after,
                limit: self.max_requests,
                remaining: 0,
            }
        }
    }
}

/// IP-based rate limiter middleware helper
pub struct IpRateLimiter {
    limiter: RateLimiter,
    /// Trust proxy headers
    trust_proxy: bool,
}

impl IpRateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            limiter: RateLimiter::new(config),
            trust_proxy: false,
        }
    }

    pub fn with_trusted_proxy(mut self, trust: bool) -> Self {
        self.trust_proxy = trust;
        self
    }

    /// Extract IP from request headers
    pub fn extract_ip(&self, headers: &http::HeaderMap, remote_addr: &str) -> String {
        if self.trust_proxy {
            // Check X-Forwarded-For
            if let Some(forwarded) = headers.get("x-forwarded-for") {
                if let Ok(forwarded_str) = forwarded.to_str() {
                    // First IP in the list is the client
                    return forwarded_str.split(',').next().unwrap_or(remote_addr).trim().to_string();
                }
            }
            
            // Check X-Real-IP
            if let Some(real_ip) = headers.get("x-real-ip") {
                if let Ok(real_ip_str) = real_ip.to_str() {
                    return real_ip_str.trim().to_string();
                }
            }
        }
        
        remote_addr.to_string()
    }

    pub async fn check(&self, ip: &str, endpoint: Option<&str>) -> RateLimitResult {
        self.limiter.check(ip, endpoint).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[tokio::test]
    async fn test_token_bucket() {
        let mut bucket = TokenBucket::new(10, 1.0);

        // Should allow up to max_tokens
        for _ in 0..10 {
            assert!(bucket.try_consume(1));
        }

        // Should deny after exhaustion
        assert!(!bucket.try_consume(1));

        // Wait for refill
        thread::sleep(Duration::from_secs(2));
        assert!(bucket.try_consume(1));
    }

    #[tokio::test]
    async fn test_rate_limiter() {
        let config = RateLimitConfig {
            max_tokens: 5,
            refill_rate: 1.0,
            window_secs: 60,
        };
        
        let limiter = RateLimiter::new(config);

        // First 5 requests should be allowed
        for i in 0..5 {
            let result = limiter.check("user-1", None).await;
            assert!(result.is_allowed(), "Request {} should be allowed", i);
        }

        // 6th request should be limited
        let result = limiter.check("user-1", None).await;
        assert!(!result.is_allowed());
        
        if let RateLimitResult::Limited { retry_after, .. } = result {
            assert!(retry_after > Duration::ZERO);
        } else {
            panic!("Expected Limited result");
        }

        // Different user should have separate limit
        let result = limiter.check("user-2", None).await;
        assert!(result.is_allowed());
    }

    #[tokio::test]
    async fn test_endpoint_limits() {
        let limiter = RateLimiter::new(RateLimitConfig::default());
        
        // Set stricter limit for /api/expensive
        limiter.set_endpoint_limit("/api/expensive", RateLimitConfig {
            max_tokens: 2,
            refill_rate: 0.1,
            window_secs: 60,
        }).await;

        // Normal endpoint
        let result = limiter.check("user-1", Some("/api/normal")).await;
        assert!(result.is_allowed());

        // Expensive endpoint - should hit limit faster
        limiter.check("user-1", Some("/api/expensive")).await;
        limiter.check("user-1", Some("/api/expensive")).await;
        let result = limiter.check("user-1", Some("/api/expensive")).await;
        assert!(!result.is_allowed());
    }

    #[tokio::test]
    async fn test_sliding_window() {
        let limiter = SlidingWindowLimiter::new(60, 5);

        // First 5 requests allowed
        for i in 0..5 {
            let result = limiter.check("user-1").await;
            assert!(result.is_allowed(), "Request {} should be allowed", i);
        }

        // 6th request denied
        let result = limiter.check("user-1").await;
        assert!(!result.is_allowed());
    }

    #[tokio::test]
    async fn test_ip_extraction() {
        let limiter = IpRateLimiter::new(RateLimitConfig::default())
            .with_trusted_proxy(true);

        let mut headers = http::HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.1, 198.51.100.1".parse().unwrap());
        headers.insert("x-real-ip", "203.0.113.1".parse().unwrap());

        let ip = limiter.extract_ip(&headers, "127.0.0.1");
        assert_eq!(ip, "203.0.113.1");

        // Without trusted proxy
        let limiter = IpRateLimiter::new(RateLimitConfig::default())
            .with_trusted_proxy(false);
        
        let ip = limiter.extract_ip(&headers, "127.0.0.1");
        assert_eq!(ip, "127.0.0.1");
    }
}
