use crate::{
    Library,
    fs::{self, Tree},
    str_field,
};
use scribe_common::{
    Result, canonical_video_url, normalize_description_links, publication_date, render_document,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};
fn source_id(v: &str) -> Option<String> {
    canonical_video_url(v).ok().map(|x| x.0)
}
fn decode(text: &str, suffix: &str) -> Option<(String, Value, String)> {
    if suffix == "json" {
        let item: Value = serde_json::from_str(text).ok()?;
        if !item.is_object()
            || item["complete"] == false
            || !item["title"].is_string()
            || !item["full_text"].is_string()
        {
            return None;
        }
        let id = source_id(str_field(&item, "source_url"))?;
        let links = normalize_description_links(&item["description_links"]);
        let known_links =
            item["description_links"].is_array() && item["description_links"] == json!(links);
        let mut extras = BTreeMap::new();
        for (k, v) in item.as_object()? {
            if ![
                "title",
                "channel",
                "published_date",
                "source_url",
                "full_text",
                "complete",
            ]
            .contains(&k.as_str())
                && !(k == "description_links" && known_links)
            {
                extras.insert(k.clone(), v.clone());
            }
        }
        let output = if extras.is_empty() {
            let mut full = item.clone();
            full["complete"] = json!(true);
            render_document(&full, "md").ok()?
        } else {
            text.to_owned()
        };
        let key = json!([
            id,
            item["title"],
            str_field(&item, "channel"),
            publication_date(&item["published_date"]),
            str_field(&item, "full_text").trim_end_matches(['\r', '\n']),
            extras,
            if known_links { links } else { vec![] }
        ]);
        return Some((id, key, output));
    }
    let pieces = text.splitn(3, "\n\n").collect::<Vec<_>>();
    if pieces.len() != 3 || pieces[0].contains('\n') || pieces[2].starts_with("PARTIAL —") {
        return None;
    }
    let title = pieces[0].strip_prefix("# ").unwrap_or(pieces[0]);
    let mut headers = BTreeMap::new();
    for line in pieces[1].lines() {
        let (k, v) = line.split_once(": ")?;
        if headers.insert(k.to_owned(), v.to_owned()).is_some() {
            return None;
        }
    }
    let id = source_id(headers.get("Source")?)?;
    let published = headers.get("Published")?;
    let mut body = pieces[2];
    let mut links = Vec::new();
    if let Some(rest) = body.strip_prefix("Description links (creator-provided):\n")
        && let Some((supplied, speech)) = rest.split_once("\n\nTranscript:\n\n")
    {
        let lines = supplied.split('\n').map(str::to_owned).collect::<Vec<_>>();
        if !lines.is_empty() && normalize_description_links(&json!(lines)) == lines {
            links = lines;
            body = speech;
        }
    }
    let extras: BTreeMap<_, _> = headers
        .iter()
        .filter(|(k, _)| !["Channel", "Published", "Source"].contains(&k.as_str()))
        .collect();
    let key = json!([
        id,
        title,
        headers.get("Channel").map(String::as_str).unwrap_or(""),
        publication_date(&json!(published)),
        body.trim_end_matches(['\r', '\n']),
        extras,
        links
    ]);
    Some((id, key, text.to_owned()))
}
impl Library {
    pub fn folder_documents(
        &self,
        jobs: &[Value],
        _jobs_root: &Path,
        folder_id: Option<&str>,
    ) -> Result<Value> {
        let base = self.scope(folder_id)?;
        let blocked: HashSet<_> = jobs
            .iter()
            .filter(|j| j["status"] == "removed")
            .map(|j| str_field(j, "video_id"))
            .collect();
        let eligible: Vec<_> = jobs
            .iter()
            .filter(|j| j["status"] == "completed" && !blocked.contains(str_field(j, "video_id")))
            .collect();
        let known: HashMap<_, _> = eligible
            .iter()
            .map(|j| (str_field(j, "video_id").to_owned(), *j))
            .collect();
        let mut identities = HashMap::new();
        let mut tracked = HashMap::new();
        // Every attempt may remember a different physical, user-edited file.
        // Choosing one metadata record per video must not discard that evidence.
        for job in eligible {
            let id = str_field(job, "video_id").to_owned();
            for p in Self::tracked(job, Path::new("/")) {
                tracked.insert(p, id.clone());
            }
            for entry in job["export_evidence"]["files"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(pair) = entry["identity"].as_array()
                    && pair.len() == 2
                    && let (Some(a), Some(b)) = (pair[0].as_u64(), pair[1].as_u64())
                {
                    identities.insert((a, b), id.clone());
                }
            }
        }
        let tree = Tree::scan(&base, true, true, BTreeSet::new())?;
        let mut records = tree
            .inventory
            .iter()
            .filter(|(_, i)| i.is_file())
            .collect::<Vec<_>>();
        records
            .sort_by_cached_key(|(p, _)| p.iter().map(|x| crate::casefold(x)).collect::<Vec<_>>());
        let mut documents = Vec::new();
        let mut seen = HashSet::new();
        let (mut duplicate_count, mut skipped_count, mut total_bytes, mut matching) = (0, 0, 0, 0);
        for (path, info) in records {
            tree.check_limit()?;
            let suffix = Path::new(path.last().unwrap())
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let remembered = identities.get(&(info.dev, info.ino));
            let expected =
                remembered.or_else(|| tracked.get(&base.join(path.iter().collect::<PathBuf>())));
            if !["txt", "md", "json"].contains(&suffix.as_str()) && remembered.is_none() {
                skipped_count += 1;
                continue;
            }
            if info.size > 8 * 1024 * 1024 {
                return Err(fs::safe(
                    "A file exceeds the 8 MiB copy limit. Choose a smaller folder; nothing was copied.",
                ));
            }
            let raw = tree.read(path, 8 * 1024 * 1024)?;
            let raw = raw.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&raw);
            let text = match std::str::from_utf8(raw) {
                Ok(t) => t.replace("\r\n", "\n"),
                Err(_) => {
                    if expected.is_some() {
                        return Err(fs::safe(
                            "A recorded transcript is not readable UTF-8. Nothing was copied.",
                        ));
                    }
                    skipped_count += 1;
                    continue;
                }
            };
            let (key, output) = if let Some((id, key, output)) = decode(&text, &suffix) {
                if !known.contains_key(&id) {
                    skipped_count += 1;
                    continue;
                }
                (key, output)
            } else if let Some(id) =
                remembered.filter(|id| known.contains_key(*id) && suffix != "json")
            {
                let pieces = text.splitn(3, "\n\n").collect::<Vec<_>>();
                if pieces.len() == 3
                    && pieces[2].starts_with("PARTIAL —")
                    && pieces[1].lines().any(|l| {
                        l.strip_prefix("Source: ").and_then(source_id).as_ref() == Some(id)
                    })
                {
                    return Err(fs::safe(
                        "A recorded transcript is marked partial. Nothing was copied.",
                    ));
                }
                let job = known[id];
                let doc = json!({"title":job["title"],"channel":job["metadata"]["channel"],"published_date":job["metadata"]["published_date"],"source_url":job["url"],"full_text":text,"complete":true});
                (
                    json!([id, text, "edited-export"]),
                    render_document(&doc, "md")?,
                )
            } else {
                if expected.is_some() {
                    return Err(fs::safe(
                        "A recorded transcript's format could not be read safely. Nothing was copied.",
                    ));
                }
                skipped_count += 1;
                continue;
            };
            matching += 1;
            if matching > 10000 {
                return Err(fs::safe(
                    "This folder contains too many transcripts to copy at once.",
                ));
            }
            let hash = hex::encode(Sha256::digest(serde_json::to_vec(&key)?));
            if !seen.insert(hash) {
                duplicate_count += 1;
                continue;
            }
            let output = format!("{}\n", output.trim_end_matches('\n'));
            total_bytes += output.len() + if documents.is_empty() { 0 } else { 7 };
            if total_bytes > 16 * 1024 * 1024 {
                return Err(fs::safe(
                    "These transcripts exceed the 16 MiB clipboard limit. Choose a smaller folder; nothing was copied.",
                ));
            }
            documents.push(output);
        }
        tree.unchanged()?;
        Ok(
            json!({"content":documents.join("\n\n---\n\n"),"count":documents.len(),"duplicate_count":duplicate_count,"skipped_count":skipped_count}),
        )
    }
}
