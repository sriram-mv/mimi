use mimi_core::{flags, Cell, Color, Emulator, TermEvent};

fn emu(cols: usize, rows: usize) -> Emulator {
    Emulator::new(cols, rows, 100)
}

fn feed(e: &mut Emulator, s: &str) {
    e.process(s.as_bytes());
}

fn screen(e: &Emulator) -> Vec<String> {
    e.term.screen_text()
}

#[test]
fn plain_text_and_wrap() {
    let mut e = emu(10, 3);
    feed(&mut e, "hello");
    assert_eq!(screen(&e)[0], "hello");
    assert_eq!((e.term.cursor_x, e.term.cursor_y), (5, 0));

    feed(&mut e, " world!!"); // 13 chars total -> wraps after col 10
    assert_eq!(screen(&e)[0], "hello worl");
    assert_eq!(screen(&e)[1], "d!!");
}

#[test]
fn crlf_moves_lines() {
    let mut e = emu(20, 3);
    feed(&mut e, "one\r\ntwo\r\nthree");
    assert_eq!(screen(&e), vec!["one", "two", "three"]);
}

#[test]
fn scrollback_on_overflow() {
    let mut e = emu(10, 2);
    feed(&mut e, "a\r\nb\r\nc\r\nd");
    assert_eq!(screen(&e), vec!["c", "d"]);
    assert_eq!(e.term.scrollback.len(), 2);
    // view_line reaches into scrollback
    let l0: String = e
        .term
        .view_line(2, 0)
        .unwrap()
        .iter()
        .map(|c| c.ch)
        .collect();
    assert!(l0.starts_with('a'));
}

#[test]
fn cursor_movement_csi() {
    let mut e = emu(10, 5);
    feed(&mut e, "\x1b[3;4Hx");
    assert_eq!((e.term.cursor_x, e.term.cursor_y), (4, 2));
    assert_eq!(e.term.grid.cell(3, 2).ch, 'x');
    feed(&mut e, "\x1b[2A\x1b[3D");
    assert_eq!((e.term.cursor_x, e.term.cursor_y), (1, 0));
}

#[test]
fn sgr_colors() {
    let mut e = emu(20, 2);
    feed(&mut e, "\x1b[31mred\x1b[0m \x1b[1;38;2;10;20;30mX");
    assert_eq!(e.term.grid.cell(0, 0).fg, Color::Indexed(1));
    let x = e.term.grid.cell(4, 0);
    assert_eq!(x.fg, Color::Rgb(10, 20, 30));
    assert_ne!(x.flags & flags::BOLD, 0);
    // colon form
    feed(&mut e, "\x1b[0m\x1b[38:5:196mY");
    assert_eq!(e.term.grid.cell(5, 0).fg, Color::Indexed(196));
}

#[test]
fn erase_line_and_display() {
    let mut e = emu(10, 3);
    feed(&mut e, "aaaaaaaaaa\r\nbbbbbbbbbb\r\ncccccccccc");
    feed(&mut e, "\x1b[2;5H\x1b[K"); // erase to end of line 2
    assert_eq!(screen(&e)[1], "bbbb");
    feed(&mut e, "\x1b[2J");
    assert_eq!(screen(&e), vec!["", "", ""]);
}

#[test]
fn alt_screen_roundtrip() {
    let mut e = emu(10, 3);
    feed(&mut e, "primary");
    feed(&mut e, "\x1b[?1049h");
    assert!(e.term.alt_active);
    assert_eq!(screen(&e), vec!["", "", ""]);
    feed(&mut e, "alt!");
    assert_eq!(screen(&e)[0], "alt!");
    feed(&mut e, "\x1b[?1049l");
    assert!(!e.term.alt_active);
    assert_eq!(screen(&e)[0], "primary");
    assert_eq!((e.term.cursor_x, e.term.cursor_y), (7, 0));
}

#[test]
fn scroll_region() {
    let mut e = emu(10, 4);
    feed(&mut e, "1\r\n2\r\n3\r\n4");
    feed(&mut e, "\x1b[2;3r"); // region rows 2-3
    feed(&mut e, "\x1b[3;1H\n"); // LF at region bottom scrolls region only
    assert_eq!(screen(&e), vec!["1", "3", "", "4"]);
}

#[test]
fn wide_chars() {
    let mut e = emu(6, 2);
    feed(&mut e, "日本");
    let c0 = e.term.grid.cell(0, 0);
    assert_eq!(c0.ch, '日');
    assert_ne!(c0.flags & flags::WIDE, 0);
    assert_ne!(e.term.grid.cell(1, 0).flags & flags::WIDE_SPACER, 0);
    assert_eq!(e.term.grid.cell(2, 0).ch, '本');
    assert_eq!(e.term.grid.row_text(0), "日本");
}

#[test]
fn title_and_bell_events() {
    let mut e = emu(10, 2);
    feed(&mut e, "\x1b]0;my title\x07\x07");
    assert_eq!(e.term.title, "my title");
    let evs: Vec<_> = e.term.events.drain(..).collect();
    assert!(evs.contains(&TermEvent::Title("my title".into())));
    assert!(evs.contains(&TermEvent::Bell));
}

#[test]
fn osc_st_terminator() {
    let mut e = emu(10, 2);
    feed(&mut e, "\x1b]2;abc\x1b\\ok");
    assert_eq!(e.term.title, "abc");
    assert_eq!(screen(&e)[0], "ok");
}

#[test]
fn cwd_via_osc7() {
    let mut e = emu(20, 2);
    feed(&mut e, "\x1b]7;file://mac.local/Users/s%20v/dev\x07");
    assert_eq!(e.term.cwd.as_deref(), Some("/Users/s v/dev"));
}

#[test]
fn semantic_blocks_full_cycle() {
    let mut e = emu(40, 5);
    // prompt, command reported, executed, output, finished
    feed(&mut e, "\x1b]133;A\x07$ ");
    feed(&mut e, "\x1b]633;E;cargo build\x07");
    feed(&mut e, "\x1b]133;C\x07");
    feed(&mut e, "Compiling mimi\r\nFinished dev\r\n");
    feed(&mut e, "\x1b]133;D;0\x07");
    feed(&mut e, "\x1b]133;A\x07$ ");

    assert_eq!(e.term.blocks.len(), 1);
    let b = e.term.blocks.last().unwrap();
    assert_eq!(b.command, "cargo build");
    assert_eq!(b.exit, Some(0));
    assert!(!b.running);
    assert!(b.output.contains("Compiling mimi\n"));
    assert!(b.output.contains("Finished dev"));

    let evs: Vec<_> = e.term.events.drain(..).collect();
    assert!(evs.contains(&TermEvent::BlockStarted(0)));
    assert!(evs.contains(&TermEvent::BlockFinished(0)));
}

#[test]
fn blocks_escaped_command() {
    let mut e = emu(40, 5);
    feed(
        &mut e,
        "\x1b]633;E;echo a\\x3b echo b\x07\x1b]133;C\x07\x1b]133;D;1\x07",
    );
    let b = e.term.blocks.last().unwrap();
    assert_eq!(b.command, "echo a; echo b");
    assert_eq!(b.exit, Some(1));
}

#[test]
fn alt_screen_output_not_captured() {
    let mut e = emu(40, 5);
    feed(&mut e, "\x1b]633;E;vim\x07\x1b]133;C\x07");
    feed(&mut e, "\x1b[?1049hTUI JUNK\x1b[?1049l");
    feed(&mut e, "done\r\n\x1b]133;D;0\x07");
    let b = e.term.blocks.last().unwrap();
    assert!(!b.output.contains("TUI JUNK"));
    assert!(b.output.contains("done"));
}

#[test]
fn dec_modes() {
    let mut e = emu(10, 3);
    feed(&mut e, "\x1b[?25l\x1b[?2004h\x1b[?1h");
    assert!(!e.term.show_cursor);
    assert!(e.term.bracketed_paste);
    assert!(e.term.app_cursor);
    feed(&mut e, "\x1b[?25h\x1b[?2004l\x1b[?1l");
    assert!(e.term.show_cursor);
    assert!(!e.term.bracketed_paste);
}

#[test]
fn device_status_reports() {
    let mut e = emu(10, 3);
    feed(&mut e, "\x1b[5;7H\x1b[6n");
    let resp = std::mem::take(&mut e.term.responses);
    assert_eq!(String::from_utf8(resp).unwrap(), "\x1b[3;7R"); // clamped to rows
    feed(&mut e, "\x1b[c");
    assert!(!e.term.responses.is_empty());
}

#[test]
fn utf8_split_across_reads() {
    let mut e = emu(10, 2);
    let bytes = "é".as_bytes();
    e.process(&bytes[..1]);
    e.process(&bytes[1..]);
    assert_eq!(e.term.grid.cell(0, 0).ch, 'é');
}

#[test]
fn line_drawing_charset() {
    let mut e = emu(10, 2);
    feed(&mut e, "\x1b(0lqk\x1b(B");
    assert_eq!(screen(&e)[0], "┌─┐");
}

#[test]
fn insert_delete_chars_and_lines() {
    let mut e = emu(10, 4);
    feed(&mut e, "abcdef\x1b[1;1H\x1b[2@");
    assert_eq!(screen(&e)[0], "  abcdef");
    feed(&mut e, "\x1b[3P");
    assert_eq!(screen(&e)[0], "bcdef");
    feed(&mut e, "\x1b[2;1Hx\x1b[2;1H\x1b[1L");
    assert_eq!(screen(&e)[1], "");
    assert_eq!(screen(&e)[2], "x");
}

#[test]
fn resize_preserves_content() {
    let mut e = emu(10, 3);
    feed(&mut e, "keep");
    e.term.resize(20, 5);
    assert_eq!(screen(&e)[0], "keep");
    assert_eq!(e.term.cols(), 20);
    e.term.resize(5, 2);
    assert_eq!(e.term.rows(), 2);
}

#[test]
fn full_reset_keeps_blocks() {
    let mut e = emu(10, 3);
    feed(&mut e, "\x1b]633;E;ls\x07\x1b]133;C\x07\x1b]133;D;0\x07");
    feed(&mut e, "junk\x1bc");
    assert_eq!(screen(&e)[0], "");
    assert_eq!(e.term.blocks.len(), 1);
}

#[test]
fn clear_scrollback_csi3j() {
    let mut e = emu(5, 2);
    feed(&mut e, "a\r\nb\r\nc\r\nd");
    assert!(!e.term.scrollback.is_empty());
    feed(&mut e, "\x1b[3J");
    assert_eq!(e.term.scrollback.len(), 0);
}

#[test]
fn default_cell_is_space() {
    assert_eq!(Cell::default().ch, ' ');
    assert_eq!(Cell::default().fg, Color::Default);
}
