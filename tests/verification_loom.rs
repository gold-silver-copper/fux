#![allow(clippy::expect_used, clippy::panic)]

#[path = "verify/loom/mod.rs"]
mod models;

#[test]
fn manager_election_has_exactly_one_first_client_winner() {
    models::manager_election();
}

#[test]
fn snapshot_and_notification_never_publish_an_uncommitted_generation() {
    models::snapshot_notification();
}

#[test]
fn natural_exit_and_explicit_close_publish_once() {
    models::exit_close();
}

#[test]
fn shutdown_rejects_a_queued_control_mutation() {
    models::shutdown_mutation();
}

#[test]
fn subscriber_close_and_publication_do_not_resurrect_the_subscriber() {
    models::subscriber_close();
}

#[test]
fn notification_completion_and_shutdown_join_one_owner() {
    models::notification_completion();
}

#[test]
fn process_reaping_invalidates_the_signal_generation() {
    models::signal_reap();
}
