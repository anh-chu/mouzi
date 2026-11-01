use crate::{db, operations as op, rules};
use once_cell::sync::Lazy;
use std::{
    fs,
    path::PathBuf,
    sync::{Mutex, MutexGuard},
};

static DATABASE: Lazy<Mutex<()>> = Lazy::new(|| {
    let path = std::env::temp_dir().join(format!("mouzi-beta-tests-db-{}", op::new_run_id()));
    fs::create_dir_all(&path).unwrap();
    db::init_db(path).unwrap();
    Mutex::new(())
});
struct Fixture {
    root: PathBuf,
    _guard: MutexGuard<'static, ()>,
}
impl Fixture {
    fn new() -> Self {
        let guard = DATABASE.lock().unwrap_or_else(|p| p.into_inner());
        db::get_db().lock().unwrap().execute_batch("DELETE FROM rules; DELETE FROM watched_folders; DELETE FROM folder_baseline; DELETE FROM action_logs; DELETE FROM processed_files;").unwrap();
        let root = std::env::temp_dir().join(format!("mouzi-beta-fixture-{}", op::new_run_id()));
        fs::create_dir_all(&root).unwrap();
        db::add_watched_folder(root.to_str().unwrap(), "manual").unwrap();
        Self {
            root,
            _guard: guard,
        }
    }
    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, contents).unwrap();
        path
    }
    fn rule(&self) -> db::Rule {
        serde_json::from_value(serde_json::json!({"id":null,"name":"Documents","priority":0,"enabled":true,"extensions":["txt"],"pattern":null,"destination":"Sorted","action":"move","folder_id":0})).unwrap()
    }
    fn apply_all(&self) -> Vec<op::OperationResult> {
        let p = op::preview(None).unwrap();
        op::apply(
            &p.id,
            &p.entries
                .iter()
                .filter(|e| e.error.is_none())
                .map(|e| e.id.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn extension_normalization_storage_matching_and_import_atomicity() {
    let f = Fixture::new();
    let mut rule = f.rule();
    rule.extensions = vec![" exe, msi, ,".into()];
    let id = db::add_rule(&rule).unwrap();
    let stored: String = db::get_db()
        .lock()
        .unwrap()
        .query_row("SELECT extensions FROM rules WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored, "exe,msi");
    let path = f.write("readme", "a genuinely extensionless text file");
    assert!(!rules::matches_rule(
        &rules::scan_file(&path).unwrap(),
        &rule
    ));
    db::get_db()
        .lock()
        .unwrap()
        .execute("UPDATE rules SET extensions='exe, msi,' WHERE id=?1", [id])
        .unwrap();
    assert_eq!(db::get_rules().unwrap()[0].extensions, vec!["exe", "msi"]);
    assert!(rules::find_matching_rule(&rules::scan_file(&path).unwrap()).is_none());
    rule.extensions = vec!["".into()];
    assert!(db::add_rule(&rule).is_err());
    assert!(db::import_rules(&[f.rule(), rule], true).is_err());
    assert_eq!(db::get_rules().unwrap().len(), 1);
    let mut wildcard = f.rule();
    wildcard.extensions = vec!["*".into()];
    assert!(rules::matches_rule(
        &rules::scan_file(&path).unwrap(),
        &wildcard
    ));
}

#[test]
fn combined_conditions_validation_and_folder_scope() {
    let f = Fixture::new();
    let path = f.write("invoice.txt", "12345");
    let file = rules::scan_file(&path).unwrap();
    let mut rule = f.rule();
    rule.pattern = Some("^invoice".into());
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    rule.options.min_size = Some(5);
    rule.options.max_size = Some(5);
    rule.options.modified_after = Some(today.clone());
    rule.options.modified_before = Some(today);
    assert!(rules::matches_rule(&file, &rule));
    rule.options.min_size = Some(6);
    assert!(!rules::matches_rule(&file, &rule));
    assert!(rules::validate_rule(&rule).is_err());
    rule.options.min_size = Some(0);
    rule.pattern = Some("[".into());
    assert!(rules::validate_rule(&rule).is_err());
    rule.pattern = None;
    rule.options.modified_after = Some("invalid".into());
    assert!(rules::validate_rule(&rule).is_err());
    rule.options = Default::default();
    rule.folder_id = 99999;
    db::add_rule(&rule).unwrap();
    assert!(rules::find_matching_rule(&file).is_none());
}

#[test]
fn preview_selective_apply_changed_files_rules_and_conflicts() {
    let f = Fixture::new();
    let mut rule = f.rule();
    rule.id = Some(db::add_rule(&rule).unwrap());
    let a = f.write("a.txt", "alpha");
    let b = f.write("b.txt", "beta");
    let p = op::preview(None).unwrap();
    assert_eq!(p.entries.len(), 2);
    assert!(a.exists() && b.exists());
    assert!(!f.root.join("Sorted").exists());
    assert!(op::apply(&p.id, &[p.entries[0].id.clone()]).unwrap()[0].success);
    assert!(!a.exists());
    assert!(b.exists());
    assert!(op::apply(&p.id, &[]).is_err());
    let p = op::preview(None).unwrap();
    fs::write(&b, "changed").unwrap();
    assert!(!op::apply(&p.id, &[p.entries[0].id.clone()]).unwrap()[0].success);
    assert!(b.exists());
    let p = op::preview(None).unwrap();
    let dest = PathBuf::from(p.entries[0].destination.as_ref().unwrap());
    fs::write(&dest, "existing content").unwrap();
    assert!(!op::apply(&p.id, &[p.entries[0].id.clone()]).unwrap()[0].success);
    assert_eq!(fs::read_to_string(&dest).unwrap(), "existing content");
    assert!(b.exists());
    let p = op::preview(None).unwrap();
    assert!(p.entries[0]
        .destination
        .as_ref()
        .unwrap()
        .ends_with("b (1).txt"));
    rule.destination = "Elsewhere".into();
    db::update_rule(&rule).unwrap();
    assert!(!op::apply(&p.id, &[p.entries[0].id.clone()]).unwrap()[0].success);
    assert!(b.exists());
}

#[test]
fn combined_review_rejects_changed_trash_and_keeps_unselected_files() {
    let f = Fixture::new();
    db::add_rule(&f.rule()).unwrap();
    let changed = f.write("changed.txt", "original");
    let untouched = f.write("untouched.txt", "leave alone");
    let p = op::preview(None).unwrap();
    let changed_id = p.entries.iter().find(|e| e.source == changed.to_string_lossy().as_ref()).unwrap().id.clone();
    let untouched_id = p.entries.iter().find(|e| e.source == untouched.to_string_lossy().as_ref()).unwrap().id.clone();
    fs::write(&changed, "changed since review").unwrap();
    let results = op::apply_with_trash(&p.id, &[changed_id.clone()], &[changed_id, untouched_id]).unwrap();
    assert_eq!(results.len(), 1);
    assert!(!results[0].success);
    assert!(changed.exists());
    assert!(untouched.exists());
}

#[cfg(target_os = "windows")]
#[test]
fn combined_review_applies_move_and_trash_in_one_run() {
    let f = Fixture::new();
    db::add_rule(&f.rule()).unwrap();
    let moved = f.write("move.txt", "keep");
    let trashed = f.write("trash.txt", "discard");
    let p = op::preview(None).unwrap();
    let moved_id = p.entries.iter().find(|e| e.source == moved.to_string_lossy().as_ref()).unwrap().id.clone();
    let trash_id = p.entries.iter().find(|e| e.source == trashed.to_string_lossy().as_ref()).unwrap().id.clone();
    let results = op::apply_with_trash(&p.id, &[moved_id, trash_id.clone()], &[trash_id]).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.success));
    assert!(f.root.join("Sorted/move.txt").exists());
    assert!(!trashed.exists());
    let logs = db::get_recent_logs(-1).unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].run_id, logs[1].run_id);
    assert!(logs.iter().any(|l| l.file_name == "trash.txt" && l.action == "delete" && l.destination_path.is_none()));
}

#[test]
fn runs_filters_safe_partial_undo_and_no_automatic_redo() {
    let f = Fixture::new();
    db::add_rule(&f.rule()).unwrap();
    let a = f.write("a.txt", "alpha");
    let b = f.write("b.txt", "beta");
    assert!(f.apply_all().iter().all(|r| r.success));
    let logs = db::get_recent_logs(-1).unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].run_id, logs[1].run_id);
    let a_log = logs.iter().find(|l| l.file_name == "a.txt").unwrap();
    let b_log = logs.iter().find(|l| l.file_name == "b.txt").unwrap();
    fs::write(&a, "new unrelated file").unwrap();
    assert_eq!(
        op::undo_one(a_log.id.unwrap()).unwrap_err(),
        "history.conflict"
    );
    assert!(op::undo_one(b_log.id.unwrap()).unwrap());
    assert!(b.exists());
    assert!(op::already_processed(&b).unwrap());
    let mut filter = op::HistoryFilter::default();
    filter.query = "b.".into();
    filter.rule = "documents".into();
    filter.extension = ".TXT".into();
    assert_eq!(op::history(&filter).unwrap().len(), 1);
    filter.from = "2099-01-01T00:00:00Z".into();
    assert!(op::history(&filter).unwrap().is_empty());
    fs::remove_file(&a).unwrap();
    fs::write(
        a_log.destination_path.as_ref().unwrap(),
        "modified after move",
    )
    .unwrap();
    assert_eq!(
        op::undo_one(a_log.id.unwrap()).unwrap_err(),
        "history.changed"
    );
    assert!(
        !db::get_recent_logs(-1)
            .unwrap()
            .iter()
            .find(|l| l.id == a_log.id)
            .unwrap()
            .undone
    );
    let folder = db::get_watched_folders().unwrap()[0].id.unwrap();
    db::update_folder_mode(folder, "silent").unwrap();
    assert!(rules::process_file_in_run(&b, true, "test", "automatic")
        .unwrap()
        .is_none());
    assert!(b.exists());
    db::clear_logs().unwrap();
    assert!(op::already_processed(&b).unwrap());
}

#[test]
fn rename_templates_collisions_no_loops_and_undo() {
    let f = Fixture::new();
    let mut rule = f.rule();
    rule.action = "rename".into();
    rule.options.rename_template = "renamed-{stem}.{extension}".into();
    db::add_rule(&rule).unwrap();
    let source = f.write("note.txt", "original");
    f.write("renamed-note.txt", "do not overwrite");
    let p = op::preview(Some(vec![source.to_string_lossy().into()])).unwrap();
    let dest = PathBuf::from(p.entries[0].destination.as_ref().unwrap());
    assert!(dest.ends_with("renamed-note (1).txt"));
    assert!(op::apply(&p.id, &[p.entries[0].id.clone()]).unwrap()[0].success);
    assert_eq!(dest.parent(), source.parent());
    assert!(op::already_processed(&dest).unwrap());
    assert!(op::preview(Some(vec![dest.to_string_lossy().into()]))
        .unwrap()
        .entries
        .is_empty());
    let log = db::get_recent_logs(-1).unwrap().remove(0);
    assert!(op::undo_one(log.id.unwrap()).unwrap());
    assert_eq!(fs::read_to_string(&source).unwrap(), "original");
    for template in ["../{stem}", "CON.txt", "{unknown}", "a:b", "foo."] {
        assert!(op::validate_template(template).is_err(), "{template}");
    }
}

#[test]
fn only_new_baseline_modes_suggest_and_suspicious_queue() {
    let f = Fixture::new();
    let mut rule = f.rule();
    rule.extensions = vec!["*".into()];
    db::add_rule(&rule).unwrap();
    let old = f.write("old.txt", "old");
    let id = db::get_watched_folders().unwrap()[0].id.unwrap();
    db::set_only_new(id, true).unwrap();
    let new = f.write("new.txt", "new");
    assert!(db::is_baseline_file(&old));
    assert!(!db::is_baseline_file(&new));
    db::update_folder_mode(id, "suggest").unwrap();
    assert!(db::get_watched_folders().unwrap()[0].only_new);
    assert!(rules::process_file_in_run(&new, true, "test", "scheduled")
        .unwrap()
        .is_none());
    let p = op::preview(None).unwrap();
    assert_eq!(p.entries.len(), 1);
    assert!(p.entries[0].source.ends_with("new.txt"));
    let suspicious = f.write("invoice.pdf.exe", "not an actual executable");
    assert!(op::suspicious_name(&suspicious));
    let p = op::preview(None).unwrap();
    assert!(
        p.entries
            .iter()
            .find(|e| e.source.ends_with("invoice.pdf.exe"))
            .unwrap()
            .warning
    );
    db::update_folder_mode(id, "silent").unwrap();
    assert!(
        rules::process_file_in_run(&suspicious, true, "test", "automatic")
            .unwrap()
            .is_none()
    );
    assert!(suspicious.exists());
    db::set_only_new(id, false).unwrap();
    assert!(!db::is_baseline_file(&old));
    db::set_only_new(id, true).unwrap();
    assert!(db::is_baseline_file(&new));
    assert!(db::add_watched_folder(f.root.to_str().unwrap(), "manual").is_err());
}

#[cfg(windows)]
#[test]
fn moving_preserves_mark_of_the_web_and_never_overwrites() {
    let f = Fixture::new();
    let source = f.write("download.txt", "content");
    let destination = f.root.join("moved.txt");
    let mark = "[ZoneTransfer]\r\nZoneId=3\r\n";
    fs::write(format!("{}:Zone.Identifier", source.display()), mark).unwrap();
    op::move_without_overwrite(&source, &destination).unwrap();
    assert_eq!(
        fs::read_to_string(format!("{}:Zone.Identifier", destination.display())).unwrap(),
        mark
    );
    let source = f.write("download.txt", "another");
    assert!(op::move_without_overwrite(&source, &destination).is_err());
    assert!(source.exists());
}
