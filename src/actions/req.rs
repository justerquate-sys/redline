//! InnerTube request bodies for the actions (without `context`, added by
//! `with_context`) and response classification. Pure, unit-tested.

use super::watch::Like;
use crate::config::Config;
use crate::json::{self, Value as In};
use serde_json::{Value, json};

pub type Body = (&'static str, Value);

/// `like/like`, `like/dislike` or `like/removelike` for a video. `params` are
/// the opaque `likeParams`/`dislikeParams`/`removeLikeParams` from the page.
pub fn like(v: &str, to: Like, params: Option<&str>) -> Body {
    let ep = match to {
        Like::Like => "like/like",
        Like::Dislike => "like/dislike",
        Like::None => "like/removelike",
    };
    let mut b = json!({"target": {"videoId": v}});
    if let Some(p) = params.filter(|p| !p.is_empty()) {
        b["params"] = p.into();
    }
    (ep, b)
}

pub fn subscribe(channel: &str, on: bool, params: Option<&str>) -> Body {
    let mut b = json!({"channelIds": [channel]});
    if let Some(p) = params.filter(|p| !p.is_empty()) {
        b["params"] = p.into();
    }
    (if on { "subscription/subscribe" } else { "subscription/unsubscribe" }, b)
}

/// The user's playlists with this video's membership.
pub fn playlists(v: &str) -> Body {
    ("playlist/get_add_to_playlist", json!({"videoIds": [v], "excludeWatchLater": false}))
}

pub fn edit_playlist(list: &str, v: &str, add: bool) -> Body {
    let action = if add { json!({"addedVideoId": v, "action": "ACTION_ADD_VIDEO"}) } else { json!({"action": "ACTION_REMOVE_VIDEO_BY_VIDEO_ID", "removedVideoId": v}) };
    ("browse/edit_playlist", json!({"playlistId": list, "actions": [action]}))
}

/// A new private playlist containing `v`.
pub fn create_playlist(title: &str, v: &str) -> Body {
    ("playlist/create", json!({"title": title, "privacyStatus": "PRIVATE", "videoIds": [v]}))
}

/// TV: what the account says about a video (its rating, its channel): the same `next` the account's own client asks.
pub fn account_video(v: &str) -> Body {
    ("next", json!({"videoId": v}))
}

/// TV: what the account says about a channel (its subscription).
pub fn account_channel(ch: &str) -> Body {
    ("browse", json!({"browseId": ch}))
}

/// Comments, replies, sort changes and their pages are all `next` continuations.
pub fn comments(token: &str) -> Body {
    ("next", json!({"continuation": token}))
}

pub fn notifications(ctoken: Option<&str>) -> Body {
    match ctoken {
        Some(t) => ("notification/get_notification_menu", json!({"ctoken": t})),
        None => ("notification/get_notification_menu", json!({"notificationsMenuRequestType": "NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX"})),
    }
}

/// Marks one notification read (its `serializedInteractionsRequest`).
pub fn record_interaction(serialized: &str) -> Body {
    ("notification/record_interactions", json!({"serializedRecordNotificationInteractionsRequest": serialized}))
}

pub fn account_menu() -> Body {
    ("account/account_menu", json!({"deviceTheme": "DEVICE_THEME_SUPPORTED", "userInterfaceTheme": "USER_INTERFACE_THEME_DARK"}))
}

/// Full JSON body: `extra` plus the WEB `context`.
pub fn with_context(mut extra: Value, cfg: &Config, hl: &str, page_url: &str) -> String {
    extra["context"] = crate::api::context(cfg, hl, page_url);
    extra.to_string()
}

pub fn url(ep: &str) -> String {
    format!("{}/youtubei/v1/{ep}?prettyPrint=false", crate::config::ORIGIN)
}

/// A readable reason for a failed action (shown next to the rolled-back control).
pub fn failure(status: u32, json: Option<&In>) -> String {
    let msg: String = json.and_then(|j| j.pointer("/error/message")).and_then(In::as_str).unwrap_or("").chars().take(140).collect();
    let base = match status {
        0 => return "Network error: YouTube could not be reached.".into(),
        200..=299 => "YouTube did not confirm the change".to_string(),
        401 => "Sign-in required (HTTP 401)".to_string(),
        403 => "YouTube refused the request (HTTP 403)".to_string(),
        429 => "Too many requests (HTTP 429)".to_string(),
        500..=599 => format!("YouTube is unavailable (HTTP {status})"),
        _ => format!("YouTube rejected the request (HTTP {status})"),
    };
    if msg.is_empty() { base } else { format!("{base}: {msg}") }
}

/// The parsed response of a successful call, or the failure reason.
pub fn outcome(status: u32, body: &[u8]) -> Result<In<'_>, String> {
    let json = json::parse(body);
    match json {
        Some(j) if status == 200 && j.get("error").is_none() => Ok(j),
        j => Err(failure(if status == 200 && j.is_none() { 299 } else { status }, j.as_ref())),
    }
}

/// `edit_playlist` succeeded (or the video already was / wasn't there).
pub fn edit_ok(j: &In) -> bool {
    if j.get("status").and_then(In::as_str) == Some("STATUS_SUCCEEDED") {
        return true;
    }
    let s = j.to_string().to_ascii_lowercase();
    s.contains("already") || s.contains("not in playlist")
}

/// `subscription/*` succeeded: no explicit contrary state in the response.
pub fn subscribe_ok(j: &In, on: bool) -> bool {
    let mut contrary = false;
    super::walk(j, &mut |k, v| {
        if k == "updateSubscribeButtonAction" || k == "subscriptionStateEntity" {
            if let Some(b) = v.get("subscribed").and_then(In::as_bool) {
                contrary |= b != on;
            }
        }
    });
    !contrary
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bodies() {
        let (ep, b) = like("abcdefghijk", Like::Like, Some("Cg0"));
        assert_eq!((ep, b["target"]["videoId"].as_str(), b["params"].as_str()), ("like/like", Some("abcdefghijk"), Some("Cg0")));
        let (ep, b) = like("abcdefghijk", Like::None, None);
        assert_eq!(ep, "like/removelike");
        assert!(b.get("params").is_none());
        assert_eq!(like("x", Like::Dislike, Some("")).0, "like/dislike");
        let (ep, b) = subscribe("UCx", true, None);
        assert_eq!((ep, b["channelIds"][0].as_str()), ("subscription/subscribe", Some("UCx")));
        assert_eq!(subscribe("UCx", false, Some("p")).0, "subscription/unsubscribe");
        let (ep, b) = edit_playlist("PL1", "v", true);
        assert_eq!(ep, "browse/edit_playlist");
        assert_eq!(b, json!({"playlistId": "PL1", "actions": [{"addedVideoId": "v", "action": "ACTION_ADD_VIDEO"}]}));
        assert_eq!(edit_playlist("WL", "v", false).1["actions"][0], json!({"action": "ACTION_REMOVE_VIDEO_BY_VIDEO_ID", "removedVideoId": "v"}));
        assert_eq!(create_playlist("a\"b", "v").1, json!({"title": "a\"b", "privacyStatus": "PRIVATE", "videoIds": ["v"]}));
        assert_eq!(playlists("v").1["videoIds"][0], "v");
        assert_eq!(account_video("v"), ("next", json!({"videoId": "v"})));
        assert_eq!(account_channel("UCx"), ("browse", json!({"browseId": "UCx"})));
        assert_eq!(notifications(None).1["notificationsMenuRequestType"], "NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX");
        assert_eq!(notifications(Some("t")).1["ctoken"], "t");
        let full: Value = serde_json::from_str(&with_context(comments("tok").1, &Config::default(), "de", "/watch?v=x")).unwrap();
        assert_eq!(full["continuation"], "tok");
        assert_eq!(full["context"]["client"]["clientName"], "WEB");
        assert_eq!(full["context"]["client"]["hl"], "de");
        assert_eq!(url("like/like"), "https://www.youtube.com/youtubei/v1/like/like?prettyPrint=false");
    }
    #[test]
    fn outcomes() {
        assert!(outcome(200, b"{}").is_ok());
        assert_eq!(outcome(0, b"").unwrap_err(), "Network error: YouTube could not be reached.");
        assert_eq!(outcome(401, br#"{"error":{"message":"Login required"}}"#).unwrap_err(), "Sign-in required (HTTP 401): Login required");
        assert_eq!(outcome(200, b"not json").unwrap_err(), "YouTube did not confirm the change");
        assert!(outcome(200, br#"{"error":{"code":400}}"#).is_err());
        assert!(edit_ok(&crate::json::jv!({"status": "STATUS_SUCCEEDED"})));
        assert!(!edit_ok(&crate::json::jv!({"status": "STATUS_FAILED"})));
        assert!(subscribe_ok(&crate::json::jv!({"actions": [{"updateSubscribeButtonAction": {"subscribed": true}}]}), true));
        assert!(!subscribe_ok(&crate::json::jv!({"actions": [{"updateSubscribeButtonAction": {"subscribed": false}}]}), true));
        assert!(subscribe_ok(&crate::json::jv!({}), false));
    }
}
