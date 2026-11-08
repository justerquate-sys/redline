//! Isolated developer benchmark. All input is generated; no user data or native actions.
use scribe_common::render_document;
use scribe_library::Library;
use serde_json::json;
use std::{fs, time::Instant};

#[test]
#[ignore = "isolated synthetic inventory benchmark; run --release --ignored --nocapture"]
fn synthetic_library_read_paths() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let data = base.join("private");
    let jobs_root = data.join("jobs");
    let root = base.join("Transcripts");
    fs::create_dir_all(&jobs_root).unwrap();
    fs::create_dir(&root).unwrap();
    let library = Library::open(&data).unwrap();
    library.set_directory(&root).unwrap();
    let mut jobs = Vec::new();
    for folder in 0..12 {
        fs::create_dir(root.join(format!("Folder {folder:02}"))).unwrap();
    }
    for index in 0..120 {
        let id = format!("{index:032x}");
        let video = format!("v{index:010}");
        let doc = json!({"title":format!("Synthetic video {index}"),"channel":"Offline benchmark","source_url":format!("https://www.youtube.com/watch?v={video}"),"full_text":"Generated words only. ".repeat(400),"complete":true});
        fs::create_dir(jobs_root.join(&id)).unwrap();
        fs::write(
            jobs_root.join(&id).join("transcript.json"),
            serde_json::to_vec(&doc).unwrap(),
        )
        .unwrap();
        let path = root.join(format!("Folder {:02}/{index}.txt", index % 12));
        fs::write(&path, render_document(&doc, "txt").unwrap()).unwrap();
        jobs.push(json!({"id":id,"video_id":video,"status":"completed","export_path":path,"export_paths":[path],"updated_at":"initial"}));
    }
    for _ in 0..2 {
        library.file_stats(&jobs, &jobs_root, None).unwrap();
        library.export_folders().unwrap();
    }
    let mut measures = Vec::new();
    for metric in [
        "folders",
        "stats_warm",
        "stats_fresh",
        "saved_verification",
        "batch_saved_verification",
        "copy",
    ] {
        let mut samples = Vec::new();
        for round in 0..12 {
            if metric == "stats_fresh" {
                jobs[0]["updated_at"] = json!(round);
            }
            let start = Instant::now();
            match metric {
                "folders" => {
                    assert_eq!(
                        library.export_folders().unwrap()["folders"]
                            .as_array()
                            .unwrap()
                            .len(),
                        12
                    );
                }
                "stats_warm" | "stats_fresh" => {
                    assert_eq!(
                        library.file_stats(&jobs, &jobs_root, None).unwrap()["saved_here"],
                        120
                    );
                }
                "saved_verification" => {
                    assert_eq!(
                        jobs.iter()
                            .filter(|j| library.is_saved_anywhere(j, &jobs_root))
                            .count(),
                        120
                    );
                }
                "batch_saved_verification" => {
                    let facts = library.saved_job_facts(&jobs, &jobs_root);
                    assert_eq!(facts.values().filter(|saved| **saved).count(), 120);
                }
                "copy" => {
                    assert_eq!(
                        library.folder_documents(&jobs, &jobs_root, None).unwrap()["count"],
                        120
                    );
                }
                _ => unreachable!(),
            }
            samples.push(start.elapsed().as_secs_f64() * 1000.);
        }
        samples.sort_by(f64::total_cmp);
        measures.push(json!({"metric":metric,"median_ms":(samples[5]+samples[6])/2.,"mean_ms":samples.iter().sum::<f64>()/samples.len() as f64}));
    }
    println!(
        "LIBRARY_BENCH {}",
        json!({"files":120,"folders":12,"samples":12,"measurements":measures})
    );
}
