/*
 * Integration tests for the trunk ancestry check fix.
 *
 * Two bugs were fixed:
 * 1. check_parent_on_trunk in jj.rs had an inverted graph_descendant_of call
 * 2. directly_based_on_master in diff.rs only checked exact OID equality
 *
 * These tests verify both fixes work correctly.
 */

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn setup_jj_repo() -> (TempDir, std::path::PathBuf) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let repo_path = temp_dir.path().to_path_buf();

    let output = Command::new("jj")
        .args(["git", "init", "--colocate"])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to run jj git init");
    assert!(
        output.status.success(),
        "Failed to init jj repo: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = Command::new("jj")
        .args(["config", "set", "--repo", "user.name", "Test"])
        .current_dir(&repo_path)
        .output();
    let _ = Command::new("jj")
        .args(["config", "set", "--repo", "user.email", "test@test.com"])
        .current_dir(&repo_path)
        .output();

    (temp_dir, repo_path)
}

fn jj_commit(repo_path: &Path, filename: &str, content: &str, message: &str) {
    fs::write(repo_path.join(filename), content).expect("Failed to write file");
    let output = Command::new("jj")
        .args(["commit", "-m", message])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj commit");
    assert!(
        output.status.success(),
        "Failed to create commit '{}': {}",
        message,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn get_commit_oid(repo_path: &Path, revision: &str) -> String {
    let output = Command::new("jj")
        .args(["log", "-r", revision, "--no-graph", "-T", "commit_id"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to get commit OID");
    assert!(
        output.status.success(),
        "Failed to resolve revision '{}': {}",
        revision,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Invalid UTF-8")
        .trim()
        .to_string()
}

/// Set the main bookmark on a revision AND create a fake origin/main remote
/// tracking ref so that jj's `trunk()` revset resolves to a real commit OID
/// (without this, trunk() resolves to the null root commit).
fn set_main_bookmark_with_remote(repo_path: &Path, revision: &str) {
    // Set the local bookmark
    let output = Command::new("jj")
        .args(["bookmark", "set", "main", "-r", revision])
        .current_dir(repo_path)
        .output()
        .expect("Failed to set bookmark");
    assert!(
        output.status.success(),
        "Failed to set main bookmark: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Get the git OID for this revision
    let oid = get_commit_oid(repo_path, revision);

    // Create refs/remotes/origin/main so trunk() resolves via the remote ref
    let output = Command::new("git")
        .args(["update-ref", "refs/remotes/origin/main", &oid])
        .current_dir(repo_path)
        .output()
        .expect("Failed to create remote ref");
    assert!(
        output.status.success(),
        "Failed to create origin/main ref: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Import the new ref into jj
    let output = Command::new("jj")
        .args(["git", "import"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to import git refs");
    assert!(
        output.status.success(),
        "jj git import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn create_jujutsu_and_config(repo_path: &Path) -> (jj_spr::jj::Jujutsu, jj_spr::config::Config) {
    let git_repo = git2::Repository::open(repo_path).expect("Failed to open git repository");
    let jj = jj_spr::jj::Jujutsu::new(git_repo).expect("Failed to create Jujutsu instance");
    let config = jj_spr::config::Config::new(
        "test_owner".into(),
        "test_repo".into(),
        "origin".into(),
        "main".into(),
        "spr/test/".into(),
        false,
        repo_path.to_path_buf(),
    );
    (jj, config)
}

/// Parent is exactly the trunk tip -- should pass.
#[test]
fn test_parent_on_trunk_tip_passes() {
    let (_temp_dir, repo_path) = setup_jj_repo();

    // Create a commit that lands on trunk
    jj_commit(&repo_path, "trunk.txt", "trunk content", "Trunk commit");

    // Set main bookmark + remote tracking ref on @- (the commit we just created)
    set_main_bookmark_with_remote(&repo_path, "@-");

    // Create a child commit on top of trunk
    jj_commit(&repo_path, "child.txt", "child content", "Child commit");

    // The child is @-, its parent is @-- which is the trunk tip
    let parent_oid_str = get_commit_oid(&repo_path, "@--");

    let (jj, config) = create_jujutsu_and_config(&repo_path);

    let parent_oid = git2::Oid::from_str(&parent_oid_str).expect("Failed to parse parent OID");

    let result = jj.check_parent_on_trunk(parent_oid, &config);
    assert!(
        result.is_ok(),
        "check_parent_on_trunk should pass when parent is trunk tip, got: {:?}",
        result.err()
    );
}

/// Parent is an ancestor of trunk tip (not equal) -- should still pass.
/// This is the key regression test: the old code only checked equality.
#[test]
fn test_parent_on_trunk_ancestor_passes() {
    let (_temp_dir, repo_path) = setup_jj_repo();

    // Create commit A on trunk
    jj_commit(&repo_path, "a.txt", "content a", "Commit A on trunk");
    let commit_a_oid_str = get_commit_oid(&repo_path, "@-");

    // Create commit B on trunk (B is now trunk tip, A is trunk~1)
    jj_commit(&repo_path, "b.txt", "content b", "Commit B on trunk");

    // Set main bookmark + remote ref on @- (commit B, the latest trunk commit)
    set_main_bookmark_with_remote(&repo_path, "@-");

    let trunk_tip_str = get_commit_oid(&repo_path, "@-");
    println!("Commit A OID: {}", commit_a_oid_str);
    println!("Trunk tip (B) OID: {}", trunk_tip_str);
    assert_ne!(commit_a_oid_str, trunk_tip_str, "A and B should differ");

    // Create a child whose parent is commit A (not trunk tip B).
    let output = Command::new("jj")
        .args(["new", &commit_a_oid_str])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to run jj new");
    assert!(
        output.status.success(),
        "jj new failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    jj_commit(&repo_path, "child.txt", "child content", "Child based on A");

    let (jj, config) = create_jujutsu_and_config(&repo_path);

    let commit_a_oid =
        git2::Oid::from_str(&commit_a_oid_str).expect("Failed to parse commit A OID");

    // is_on_trunk should return true for commit A (ancestor of trunk tip)
    let on_trunk = jj
        .is_on_trunk(commit_a_oid)
        .expect("is_on_trunk should not error");
    assert!(
        on_trunk,
        "Commit A is an ancestor of trunk tip and should be recognized as on trunk"
    );

    // check_parent_on_trunk should also pass
    let result = jj.check_parent_on_trunk(commit_a_oid, &config);
    assert!(
        result.is_ok(),
        "check_parent_on_trunk should pass for trunk ancestor, got: {:?}",
        result.err()
    );
}

/// Parent is on a side branch (not on trunk) -- should fail.
#[test]
fn test_parent_off_trunk_fails() {
    let (_temp_dir, repo_path) = setup_jj_repo();

    // Create a trunk commit
    jj_commit(&repo_path, "trunk.txt", "trunk content", "Trunk commit");

    // Set main bookmark + remote ref on trunk commit
    set_main_bookmark_with_remote(&repo_path, "@-");

    // Get the root commit (parent of trunk commit) to branch off of
    let root_oid_str = get_commit_oid(&repo_path, "root()");

    // Create a side-branch commit off root
    let output = Command::new("jj")
        .args(["new", &root_oid_str])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to run jj new");
    assert!(
        output.status.success(),
        "jj new failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    jj_commit(&repo_path, "side.txt", "side content", "Side branch commit");
    let side_oid_str = get_commit_oid(&repo_path, "@-");

    // Create a child on top of the side branch
    jj_commit(
        &repo_path,
        "child.txt",
        "child content",
        "Child on side branch",
    );

    let (jj, config) = create_jujutsu_and_config(&repo_path);

    let side_oid = git2::Oid::from_str(&side_oid_str).expect("Failed to parse side branch OID");

    // is_on_trunk should return false for the side branch commit
    let on_trunk = jj
        .is_on_trunk(side_oid)
        .expect("is_on_trunk should not error");
    assert!(
        !on_trunk,
        "Side branch commit should NOT be recognized as on trunk"
    );

    // check_parent_on_trunk should fail
    let result = jj.check_parent_on_trunk(side_oid, &config);
    assert!(
        result.is_err(),
        "check_parent_on_trunk should fail for off-trunk parent"
    );
}

/// Verify that is_on_trunk returns true for a trunk ancestor even when
/// it does not match trunk tip OID. This is the scenario that
/// directly_based_on_master in diff.rs now handles via the is_on_trunk
/// fallback.
#[test]
fn test_directly_based_on_master_with_trunk_ancestor() {
    let (_temp_dir, repo_path) = setup_jj_repo();

    // Create commit A
    jj_commit(&repo_path, "a.txt", "content a", "Commit A");
    let commit_a_oid_str = get_commit_oid(&repo_path, "@-");

    // Create commit B (now trunk tip)
    jj_commit(&repo_path, "b.txt", "content b", "Commit B");

    // Set main bookmark + remote ref to B
    set_main_bookmark_with_remote(&repo_path, "@-");

    let trunk_tip_str = get_commit_oid(&repo_path, "@-");

    // Create a change whose parent is A
    let output = Command::new("jj")
        .args(["new", &commit_a_oid_str])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to run jj new");
    assert!(output.status.success());

    jj_commit(
        &repo_path,
        "feature.txt",
        "feature content",
        "Feature based on A",
    );

    let (jj, _config) = create_jujutsu_and_config(&repo_path);

    let commit_a_oid =
        git2::Oid::from_str(&commit_a_oid_str).expect("Failed to parse commit A OID");
    let trunk_tip_oid = git2::Oid::from_str(&trunk_tip_str).expect("Failed to parse trunk tip OID");

    // A != trunk tip (B)
    assert_ne!(
        commit_a_oid, trunk_tip_oid,
        "Commit A and trunk tip B should be different OIDs"
    );

    // But A IS on trunk (ancestor of B)
    let on_trunk = jj
        .is_on_trunk(commit_a_oid)
        .expect("is_on_trunk should not error");
    assert!(
        on_trunk,
        "Commit A is a trunk ancestor -- is_on_trunk must return true \
         so that directly_based_on_master evaluates correctly via the \
         is_on_trunk fallback"
    );
}
