//! Приклади в репозиторії мають компілюватись — інакше вони тихо гниють.
//!
//! `examples/cookbook` — це доки: кожна сторінка там є відповіддю на задачу,
//! і зламаний рецепт гірший за відсутній.

use std::path::PathBuf;

use rhaix_server::{check, Config};

fn root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples")
        .join(name)
}

fn assert_compiles(name: &str) {
    let config = Config::load_for_check(root(name)).expect("конфіг прикладу");
    let issues = check(&config);
    assert!(
        issues.is_empty(),
        "`examples/{name}` не компілюється:\n{}",
        issues
            .iter()
            .map(|issue| issue.rendered.clone())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_demo_compiles() {
    assert_compiles("demo");
}

#[test]
fn every_recipe_compiles() {
    assert_compiles("cookbook");
}
