//! Snapshot tests for CLI output.

#[test]
fn help_text_top_level() {
    use clap::CommandFactory;
    let mut cmd = grs::commands::Args::command();
    let help = cmd.render_help();
    insta::assert_snapshot!(help.to_string());
}
