#[test]
fn ignores_commented_and_escaped() {
    // A commented \usepackage is not seen; an escaped \% is not a comment.
    let commands = scan_commands("\\usepackage{amsmath} % \\usepackage{ignored}\n");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].1.required, vec!["amsmath"]);
    let commands = scan_commands(r"\text{\%} \usepackage{keep}");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].1.required, vec!["keep"]);
}

#[test]
fn parses_packages_and_options() {
    let commands = scan_commands(r"\usepackage[utf8]{inputenc}\RequirePackage{amsmath,graphicx}");
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].1.name, "usepackage");
    assert_eq!(commands[0].1.options, vec!["utf8"]);
    assert_eq!(commands[0].1.required, vec!["inputenc"]);
    assert_eq!(commands[1].1.required, vec!["amsmath,graphicx"]);
}

#[test]
fn local_registry_indexes_only_installed_runfiles() {
    let root = temporary_test_root("installed-registry-subset");
    let tlpdb = root.join("tlpkg/texlive.tlpdb");
    fs::create_dir_all(tlpdb.parent().unwrap()).unwrap();
    let metadata = concat!(
        "name installed\n",
        "category Package\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/installed/installed.sty\n",
        "\n",
        "name absent\n",
        "category Package\n",
        "revision 2\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/absent/absent.sty\n",
    );
    fs::write(&tlpdb, metadata).unwrap();
    let installed = root.join("texmf-dist/tex/latex/installed/installed.sty");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    fs::write(installed, b"% installed").unwrap();
    let mut index = TlpdbIndex::load(&tlpdb).unwrap();
    let original_digest = index.metadata_digest().to_string();

    index.retain_installed_runfiles();

    assert_eq!(index.provider_of_file("installed.sty"), Some("installed"));
    assert_eq!(index.provider_of_file("absent.sty"), None);
    assert_ne!(index.metadata_digest(), original_digest);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn parses_bare_input_argument() {
    let commands = scan_commands(r"\input sections/intro");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].1.name, "input");
    assert_eq!(commands[0].1.required, vec!["sections/intro"]);
}

#[test]
fn parses_multiline_usepackage_with_line_numbers() {
    // Arguments split across lines, with a comment in the gap.
    let text = "\\documentclass{article}\n\\usepackage[\n  colorlinks\n]% opt\n{hyperref}\n";
    let commands = scan_commands(text);
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[1].1.name, "usepackage");
    assert_eq!(commands[1].1.options, vec!["colorlinks"]);
    assert_eq!(commands[1].1.required, vec!["hyperref"]);
    assert_eq!(commands[1].0, 2, "line of the \\usepackage backslash");
}

#[test]
fn skips_verbatim_and_verb() {
    let text = "\\begin{verbatim}\n\\usepackage{nope}\n\\end{verbatim}\n\
\\verb|\\usepackage{alsonope}|\n\\usepackage{real}\n";
    let commands = scan_commands(text);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].1.required, vec!["real"]);
}

#[test]
fn finds_nested_command_in_unknown_arg() {
    // \input inside an unknown command's braces must still be discovered.
    let commands = scan_commands(r"\textbf{\input{nested}}");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].1.name, "input");
    assert_eq!(commands[0].1.required, vec!["nested"]);
}

#[test]
fn argument_scan_stops_at_blank_line() {
    // A blank line between command and brace means no argument is taken.
    let commands = scan_commands("\\usepackage\n\n{toolate}");
    assert_eq!(commands.len(), 1);
    assert!(commands[0].1.required.is_empty());
}
use std::fs;

use crate::tests::temporary_test_root;
use crate::{TlpdbIndex, scan_commands};
