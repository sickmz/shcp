//! The pure parts of moving around the file list.
use super::{copy_name, first_starting_with, is_inside, moved_cursor, parse_mode, split_path};
use std::collections::HashSet;

fn names(list: &[&str]) -> HashSet<String> {
    list.iter().map(ToString::to_string).collect()
}

#[test]
fn a_copy_keeps_its_name_if_free_and_gets_a_new_one_if_not() {
    assert_eq!(copy_name("a.txt", &names(&["b"])), "a.txt");
    assert_eq!(copy_name("a.txt", &names(&["a.txt"])), "a copy.txt");
    assert_eq!(
        copy_name("a.txt", &names(&["a.txt", "a copy.txt"])),
        "a copy 2.txt"
    );
    assert_eq!(copy_name("dir", &names(&["dir"])), "dir copy");
    assert_eq!(copy_name(".env", &names(&[".env"])), ".env copy");
    assert_eq!(
        copy_name("a.tar.gz", &names(&["a.tar.gz"])),
        "a.tar copy.gz"
    );
}

#[test]
fn paths_split_into_folder_and_name() {
    assert_eq!(split_path("/srv/app/log"), ("/srv/app", "log"));
    assert_eq!(split_path("/srv/"), ("/", "srv"));
    assert_eq!(split_path("/top"), ("/", "top"));
}

#[test]
fn a_folder_cannot_go_inside_itself() {
    assert!(is_inside("/srv/app", "/srv/app"));
    assert!(is_inside("/srv/app/sub", "/srv/app"));
    assert!(!is_inside("/srv/application", "/srv/app"));
    assert!(!is_inside("/srv", "/srv/app"));
}

#[test]
fn permissions_are_read_in_octal() {
    assert_eq!(parse_mode("755"), Some(0o755));
    assert_eq!(parse_mode(" 0644 "), Some(0o644));
    assert_eq!(parse_mode("4755"), Some(0o4755));
    assert_eq!(parse_mode("888"), None);
    assert_eq!(parse_mode("rwx"), None);
    assert_eq!(parse_mode("77777"), None);
}

#[test]
fn arrows_move_one_row_and_stop_at_the_ends() {
    assert_eq!(moved_cursor("down", Some(1), 3, 10), Some(2));
    assert_eq!(moved_cursor("down", Some(2), 3, 10), Some(2));
    assert_eq!(moved_cursor("up", Some(1), 3, 10), Some(0));
    assert_eq!(moved_cursor("up", Some(0), 3, 10), Some(0));
}

#[test]
fn without_a_cursor_keys_start_from_an_end() {
    assert_eq!(moved_cursor("down", None, 3, 10), Some(0));
    assert_eq!(moved_cursor("pagedown", None, 3, 10), Some(0));
    assert_eq!(moved_cursor("up", None, 3, 10), Some(2));
    assert_eq!(moved_cursor("pageup", None, 3, 10), Some(2));
}

#[test]
fn pages_and_ends_jump() {
    assert_eq!(moved_cursor("pagedown", Some(5), 100, 10), Some(15));
    assert_eq!(moved_cursor("pagedown", Some(95), 100, 10), Some(99));
    assert_eq!(moved_cursor("pageup", Some(5), 100, 10), Some(0));
    assert_eq!(moved_cursor("home", Some(5), 100, 10), Some(0));
    assert_eq!(moved_cursor("end", Some(5), 100, 10), Some(99));
}

#[test]
fn an_empty_list_has_nowhere_to_go() {
    assert_eq!(moved_cursor("down", None, 0, 10), None);
    assert_eq!(moved_cursor("end", Some(3), 0, 10), None);
}

#[test]
fn typing_finds_the_first_name_ignoring_case() {
    let names = ["alpha", "Beta", "bravo"];
    assert_eq!(first_starting_with(names.into_iter(), "b"), Some(1));
    assert_eq!(first_starting_with(names.into_iter(), "BR"), Some(2));
    assert_eq!(first_starting_with(names.into_iter(), "z"), None);
}
