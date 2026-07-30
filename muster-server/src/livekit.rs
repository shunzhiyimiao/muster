//! LiveKit 入会令牌签发。
//!
//! **媒体面不经过 muster-server**——音视频直接在客户端与自托管的 LiveKit
//! 之间走,服务端只负责签一张"你可以进这个房间"的令牌。这样音视频流量
//! 不必绕经业务服务,而准入仍然由 Muster 的权限内核决定。
//!
//! ## 令牌里那几个开关就是权限的落地
//!
//! `can_publish` / `can_subscribe` 不是配置项,是 [`muster_identity::can()`]
//! 判定结果的直接映射:能在频道发言的人才能开麦,只读的人只能听。
//! 把它写死成 true 等于把权限内核绕过去了。
//!
//! ## 密级
//!
//! 会议密级 ≥ restricted 时**禁止录制**(`can_record = false`)。录像是一份
//! 长期留存、极易被搬走的正文;高密级会议连"能不能录"都不该是与会者的选择。

use serde::{Deserialize, Serialize};

use crate::ServerError;

/// LiveKit 的 VideoGrant 子集(只放我们真正用到的字段)。
#[derive(Debug, Serialize, Deserialize)]
pub struct VideoGrant {
    pub room: String,
    #[serde(rename = "roomJoin")]
    pub room_join: bool,
    #[serde(rename = "canPublish")]
    pub can_publish: bool,
    #[serde(rename = "canSubscribe")]
    pub can_subscribe: bool,
    #[serde(rename = "canPublishData")]
    pub can_publish_data: bool,
    #[serde(rename = "roomRecord")]
    pub room_record: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LkClaims {
    /// LiveKit API key
    pub iss: String,
    /// 参会者身份(用 Muster 账号 id,好让转写的说话人能对上人)
    pub sub: String,
    /// 展示名
    pub name: String,
    pub nbf: i64,
    pub exp: i64,
    pub video: VideoGrant,
}

pub struct LiveKitConfig {
    pub url: String,
    pub api_key: String,
    pub api_secret: String,
}

impl LiveKitConfig {
    /// 从环境读。**缺失即报错,不给默认值**——默认密钥等于房间对全网开放。
    pub fn from_env() -> Result<Self, String> {
        Self::validate(
            std::env::var("LIVEKIT_URL").ok(),
            std::env::var("LIVEKIT_API_KEY").ok(),
            std::env::var("LIVEKIT_API_SECRET").ok(),
        )
    }

    /// 校验单独成纯函数:可直接测,不必在测试里改进程环境
    /// (并行测试共用一个进程,谁改环境谁就在踩别人)。
    pub fn validate(
        url: Option<String>,
        api_key: Option<String>,
        api_secret: Option<String>,
    ) -> Result<Self, String> {
        let url = url.filter(|s| !s.is_empty()).ok_or("LIVEKIT_URL 未设置")?;
        let api_key = api_key.filter(|s| !s.is_empty()).ok_or("LIVEKIT_API_KEY 未设置")?;
        let api_secret = api_secret.ok_or("LIVEKIT_API_SECRET 未设置")?;
        if api_secret.len() < 32 {
            return Err("LIVEKIT_API_SECRET 至少 32 字符".into());
        }
        Ok(Self { url, api_key, api_secret })
    }
}

/// 参会者能做什么。由权限判定 + 会议密级共同决定,调用方不许自己编。
#[derive(Debug, Clone, Copy)]
pub struct JoinCaps {
    /// 能不能开麦/开摄像头(= 能不能在该频道发言)
    pub publish: bool,
    /// 能不能录制(高密级会议一律否)
    pub record: bool,
}

pub fn mint(
    cfg: &LiveKitConfig,
    room: &str,
    account_id: &str,
    display_name: &str,
    caps: JoinCaps,
    ttl_secs: i64,
) -> Result<String, ServerError> {
    use jsonwebtoken::{encode, EncodingKey, Header};
    let now = crate::db::now_ms() / 1000;
    let claims = LkClaims {
        iss: cfg.api_key.clone(),
        sub: account_id.to_string(),
        name: display_name.to_string(),
        nbf: now - 10, // 容一点时钟偏差,否则客户端稍慢就"令牌尚未生效"
        exp: now + ttl_secs,
        video: VideoGrant {
            room: room.to_string(),
            room_join: true,
            can_publish: caps.publish,
            // 订阅恒为真:能进这个房间就说明有权听——准入已经在上一层判过了
            can_subscribe: true,
            can_publish_data: caps.publish,
            room_record: caps.record,
        },
    };
    encode(&Header::default(), &claims, &EncodingKey::from_secret(cfg.api_secret.as_bytes()))
        .map_err(|e| ServerError::Internal(format!("LiveKit 令牌签发失败:{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{decode, DecodingKey, Validation};

    fn cfg() -> LiveKitConfig {
        LiveKitConfig {
            url: "ws://localhost:7880".into(),
            api_key: "devkey".into(),
            api_secret: "0123456789abcdef0123456789abcdef".into(),
        }
    }

    fn parse(token: &str, secret: &str) -> LkClaims {
        let mut v = Validation::default();
        v.validate_aud = false;
        decode::<LkClaims>(token, &DecodingKey::from_secret(secret.as_bytes()), &v).unwrap().claims
    }

    /// 令牌里的开关必须**如实反映**传入的能力,不得悄悄放宽。
    #[test]
    fn capabilities_are_carried_verbatim() {
        let c = cfg();
        let listener = mint(&c, "room-1", "bob", "Bob", JoinCaps { publish: false, record: false }, 600)
            .unwrap();
        let claims = parse(&listener, &c.api_secret);
        assert!(!claims.video.can_publish, "只读的人不该能开麦");
        assert!(claims.video.can_subscribe, "但必须能听——准入已在上一层判过");
        assert!(!claims.video.room_record);
        assert_eq!((claims.sub.as_str(), claims.video.room.as_str()), ("bob", "room-1"));

        let speaker = mint(&c, "room-1", "alice", "Alice", JoinCaps { publish: true, record: true }, 600)
            .unwrap();
        let claims = parse(&speaker, &c.api_secret);
        assert!(claims.video.can_publish && claims.video.room_record);
    }

    /// 用错密钥签的令牌进不去——房间准入靠的是这把钥匙,不是别的。
    #[test]
    fn token_is_bound_to_the_secret() {
        let c = cfg();
        let t = mint(&c, "r", "a", "A", JoinCaps { publish: true, record: false }, 600).unwrap();
        let mut v = Validation::default();
        v.validate_aud = false;
        let wrong = decode::<LkClaims>(&t, &DecodingKey::from_secret(b"another-secret-32-chars-long!!"), &v);
        assert!(wrong.is_err(), "换把钥匙必须验不过");
    }

    /// 不给默认密钥:默认密钥等于房间对全网开放。
    #[test]
    fn config_refuses_weak_or_missing_secret() {
        let ok = || Some("0123456789abcdef0123456789abcdef".to_string());
        let u = || Some("ws://localhost:7880".to_string());
        let k = || Some("devkey".to_string());

        assert!(LiveKitConfig::validate(None, k(), ok()).is_err(), "缺 URL 必须拒绝");
        assert!(LiveKitConfig::validate(u(), None, ok()).is_err(), "缺 key 必须拒绝");
        assert!(LiveKitConfig::validate(u(), k(), None).is_err(), "缺 secret 必须拒绝");
        assert!(
            LiveKitConfig::validate(u(), k(), Some("short".into())).is_err(),
            "弱密钥必须拒绝——房间准入全靠这把钥匙"
        );
        assert!(
            LiveKitConfig::validate(Some(String::new()), k(), ok()).is_err(),
            "空串等同未设置,不能当成有效配置"
        );
        assert!(LiveKitConfig::validate(u(), k(), ok()).is_ok());
    }
}
