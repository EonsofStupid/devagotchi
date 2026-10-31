use std::path::Path;

use devagotchi_contracts::{DirectoryInput, FileInfoInput, FileMoveInput};

use super::*;

fn scope(workspace: &Path, policy: ExecPolicy) -> Scope<'_> {
    Scope { workspace, policy }
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

fn moving(source: &Path, destination: &Path, overwrite: bool) -> FileMoveInput {
    FileMoveInput {
        source: text(source),
        destination: text(destination),
        overwrite,
    }
}

#[test]
fn info_says_what_a_path_is() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dunce::canonicalize(dir.path()).unwrap();
    let scope = scope(&workspace, ExecPolicy::Granted);
    let file = workspace.join("notes.txt");
    fs::write(&file, "one\ntwo\nthree").unwrap();
    let info = scope.info(FileInfoInput { path: text(&file) }).unwrap();
    assert_eq!(info.kind, "file");
    assert_eq!(info.size, 13);
    assert_eq!(info.lines, Some(3), "a last line without a newline counts");
    assert!(info.modified_ms.is_some());
    assert!(!info.readonly);
    #[cfg(unix)]
    assert!(info.mode.as_deref().is_some_and(|m| m.len() == 4), "{info:?}");

    fs::write(workspace.join("ended.txt"), "a\nb\n").unwrap();
    let ended = scope
        .info(FileInfoInput { path: text(&workspace.join("ended.txt")) })
        .unwrap();
    assert_eq!(ended.lines, Some(2));
    fs::write(workspace.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
    let blob = scope
        .info(FileInfoInput { path: text(&workspace.join("blob.bin")) })
        .unwrap();
    assert_eq!(blob.lines, None, "no lines for bytes that are not text");

    let here = scope.info(FileInfoInput { path: text(&workspace) }).unwrap();
    assert_eq!(here.kind, "dir", "the workspace itself is in reach");
    let missing = scope
        .info(FileInfoInput { path: text(&workspace.join("nothing")) })
        .unwrap_err();
    assert_eq!(missing.code, "path_not_found");
}

#[cfg(unix)]
#[test]
fn a_link_is_described_as_itself_and_must_point_inside_under_granted() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dunce::canonicalize(dir.path()).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    fs::write(workspace.join("real.txt"), "x").unwrap();
    std::os::unix::fs::symlink("real.txt", workspace.join("inside")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), workspace.join("outside")).unwrap();
    let granted = scope(&workspace, ExecPolicy::Granted);
    let inside = granted
        .info(FileInfoInput { path: text(&workspace.join("inside")) })
        .unwrap();
    assert_eq!(inside.kind, "symlink");
    assert_eq!(inside.link_target.as_deref(), Some("real.txt"));
    assert_eq!(inside.path, text(&workspace.join("inside")));
    let refused = granted
        .info(FileInfoInput { path: text(&workspace.join("outside")) })
        .unwrap_err();
    assert_eq!(refused.code, "path_outside_workspace");
    let account = scope(&workspace, ExecPolicy::Account);
    assert!(account
        .info(FileInfoInput { path: text(&workspace.join("outside")) })
        .is_ok());
}

#[test]
fn mkdir_makes_the_missing_parents_and_says_whether_it_made_anything() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dunce::canonicalize(dir.path()).unwrap();
    let scope = scope(&workspace, ExecPolicy::Granted);
    let deep = workspace.join("a").join("b").join("c");
    let made = scope.mkdir(DirectoryInput { path: text(&deep) }).unwrap();
    assert!(deep.is_dir());
    assert_eq!((made.kind.as_str(), made.created), ("dir", Some(true)));
    let again = scope.mkdir(DirectoryInput { path: text(&deep) }).unwrap();
    assert_eq!(again.created, Some(false), "an existing directory is not an error");

    fs::write(workspace.join("file"), "x").unwrap();
    let blocked = scope
        .mkdir(DirectoryInput { path: text(&workspace.join("file")) })
        .unwrap_err();
    assert_eq!(blocked.code, "not_a_directory");
    let under_file = scope
        .mkdir(DirectoryInput { path: text(&workspace.join("file").join("x")) })
        .unwrap_err();
    assert_ne!(under_file.code, "path_outside_workspace", "{under_file:?}");
}

#[test]
fn mkdir_stays_inside_the_workspace_under_granted() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dunce::canonicalize(dir.path()).unwrap().join("ws");
    fs::create_dir(&workspace).unwrap();
    let beside = workspace.parent().unwrap().join("beside").join("deeper");
    let refused = scope(&workspace, ExecPolicy::Granted)
        .mkdir(DirectoryInput { path: text(&beside) })
        .unwrap_err();
    assert_eq!(refused.code, "path_outside_workspace");
    assert!(!beside.parent().unwrap().exists(), "nothing was made");
    scope(&workspace, ExecPolicy::Account)
        .mkdir(DirectoryInput { path: text(&beside) })
        .unwrap();
    assert!(beside.is_dir());
}

#[test]
fn move_renames_and_never_replaces_without_being_told() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dunce::canonicalize(dir.path()).unwrap();
    let scope = scope(&workspace, ExecPolicy::Granted);
    let first = workspace.join("first.txt");
    let second = workspace.join("second.txt");
    fs::write(&first, "first").unwrap();
    let moved = scope.moved(moving(&first, &second, false)).unwrap();
    assert!(!first.exists());
    assert_eq!(fs::read_to_string(&second).unwrap(), "first");
    assert_eq!(moved.path, text(&second));
    assert_eq!(moved.replaced, Some(false));

    fs::write(&first, "again").unwrap();
    let refused = scope.moved(moving(&first, &second, false)).unwrap_err();
    assert_eq!(refused.code, "already_exists");
    assert_eq!(fs::read_to_string(&second).unwrap(), "first", "nothing changed");
    let replaced = scope.moved(moving(&first, &second, true)).unwrap();
    assert_eq!(replaced.replaced, Some(true));
    assert_eq!(fs::read_to_string(&second).unwrap(), "again");

    let folder = workspace.join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(&first, "x").unwrap();
    let onto_dir = scope.moved(moving(&first, &folder, true)).unwrap_err();
    assert_eq!(onto_dir.code, "already_exists");
    let into_itself = scope
        .moved(moving(&folder, &folder.join("inner"), false))
        .unwrap_err();
    assert_eq!(into_itself.code, "invalid_request");
    let renamed = scope
        .moved(moving(&folder, &workspace.join("renamed"), false))
        .unwrap();
    assert_eq!(renamed.kind, "dir");
    assert!(workspace.join("renamed").is_dir() && !folder.exists());
}

#[test]
fn move_stays_inside_the_workspace_under_granted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(dir.path()).unwrap();
    let workspace = root.join("ws");
    fs::create_dir(&workspace).unwrap();
    let inside = workspace.join("keep.txt");
    let outside = root.join("loose.txt");
    fs::write(&inside, "in").unwrap();
    fs::write(&outside, "out").unwrap();
    let granted = scope(&workspace, ExecPolicy::Granted);
    let out = granted
        .moved(moving(&inside, &root.join("taken.txt"), false))
        .unwrap_err();
    assert_eq!(out.code, "path_outside_workspace");
    let r#in = granted
        .moved(moving(&outside, &workspace.join("brought.txt"), false))
        .unwrap_err();
    assert_eq!(r#in.code, "path_outside_workspace");
    assert!(inside.exists() && outside.exists(), "nothing moved");
    scope(&workspace, ExecPolicy::Account)
        .moved(moving(&outside, &workspace.join("brought.txt"), false))
        .unwrap();
    assert!(workspace.join("brought.txt").exists());
}
