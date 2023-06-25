use crate::debugger::command::{
    ARG_COMMAND, BACKTRACE_COMMAND, BACKTRACE_COMMAND_SHORT, BREAK_COMMAND, BREAK_COMMAND_SHORT,
    CONTINUE_COMMAND, CONTINUE_COMMAND_SHORT, FRAME_COMMAND, HELP_COMMAND, HELP_COMMAND_SHORT,
    MEMORY_COMMAND, MEMORY_COMMAND_SHORT, REGISTER_COMMAND, REGISTER_COMMAND_SHORT, RUN_COMMAND,
    RUN_COMMAND_SHORT, STEP_INSTRUCTION_COMMAND, STEP_INTO_COMMAND, STEP_INTO_COMMAND_SHORT,
    STEP_OUT_COMMAND, STEP_OUT_COMMAND_SHORT, STEP_OVER_COMMAND, STEP_OVER_COMMAND_SHORT,
    SYMBOL_COMMAND, VAR_COMMAND,
};
use crossterm::style::Stylize;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::{Highlighter, MatchingBracketHighlighter};
use rustyline::hint::HistoryHinter;
use rustyline::history::MemHistory;
use rustyline::validate::MatchingBracketValidator;
use rustyline::{CompletionType, Config, Context, Editor};
use rustyline_derive::{Completer, Helper, Hinter, Validator};
use std::borrow::Cow;
use std::borrow::Cow::{Borrowed, Owned};

struct CommandView {
    short: Option<String>,
    long: String,
}

impl CommandView {
    fn long(&self) -> String {
        self.long.clone()
    }

    fn display_with_short(&self) -> String {
        if let Some(ref short) = self.short {
            if self.long.starts_with(short) {
                format!(
                    "{}{}",
                    short.clone().bold().underlined(),
                    &self.long[short.len()..]
                )
            } else {
                format!("{}|{}", &self.long, short.clone().bold().underlined())
            }
        } else {
            self.long()
        }
    }
}

impl From<&str> for CommandView {
    fn from(value: &str) -> Self {
        CommandView {
            short: None,
            long: value.to_string(),
        }
    }
}

impl From<(&str, &str)> for CommandView {
    fn from((short, long): (&str, &str)) -> Self {
        CommandView {
            short: Some(short.to_string()),
            long: long.to_string(),
        }
    }
}

pub struct CommandCompleter {
    commands: Vec<CommandView>,
}

impl CommandCompleter {
    fn new(commands: impl IntoIterator<Item = CommandView>) -> Self {
        Self {
            commands: commands.into_iter().collect(),
        }
    }
}

impl Completer for CommandCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        _pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let pairs = self
            .commands
            .iter()
            .filter_map(|cmd| {
                cmd.long.starts_with(line).then(|| Pair {
                    display: cmd.display_with_short(),
                    replacement: cmd.long(),
                })
            })
            .collect();
        Ok((0, pairs))
    }
}

#[derive(Helper, Completer, Hinter, Validator)]
pub struct RLHelper {
    #[rustyline(Completer)]
    completer: CommandCompleter,
    highlighter: MatchingBracketHighlighter,
    #[rustyline(Validator)]
    validator: MatchingBracketValidator,
    #[rustyline(Hinter)]
    hinter: HistoryHinter,
    pub colored_prompt: String,
}

impl Highlighter for RLHelper {
    fn highlight<'l>(&self, line: &'l str, pos: usize) -> Cow<'l, str> {
        self.highlighter.highlight(line, pos)
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> Cow<'b, str> {
        if default {
            Borrowed(&self.colored_prompt)
        } else {
            Borrowed(prompt)
        }
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Owned("\x1b[1m".to_owned() + hint + "\x1b[m")
    }

    fn highlight_char(&self, line: &str, pos: usize) -> bool {
        self.highlighter.highlight_char(line, pos)
    }
}

pub fn create_editor() -> anyhow::Result<Editor<RLHelper, MemHistory>> {
    let config = Config::builder()
        .history_ignore_space(true)
        .completion_type(CompletionType::List)
        .build();

    let h = RLHelper {
        completer: CommandCompleter::new([
            VAR_COMMAND.into(),
            ARG_COMMAND.into(),
            (CONTINUE_COMMAND_SHORT, CONTINUE_COMMAND).into(),
            FRAME_COMMAND.into(),
            (RUN_COMMAND_SHORT, RUN_COMMAND).into(),
            STEP_INSTRUCTION_COMMAND.into(),
            (STEP_INTO_COMMAND_SHORT, STEP_INTO_COMMAND).into(),
            (STEP_OUT_COMMAND_SHORT, STEP_OUT_COMMAND).into(),
            (STEP_OVER_COMMAND_SHORT, STEP_OVER_COMMAND).into(),
            SYMBOL_COMMAND.into(),
            (BREAK_COMMAND_SHORT, BREAK_COMMAND).into(),
            (BACKTRACE_COMMAND_SHORT, BACKTRACE_COMMAND).into(),
            (MEMORY_COMMAND_SHORT, MEMORY_COMMAND).into(),
            (REGISTER_COMMAND_SHORT, REGISTER_COMMAND).into(),
            (HELP_COMMAND_SHORT, HELP_COMMAND).into(),
            ("q", "quit").into(),
        ]),
        highlighter: MatchingBracketHighlighter::new(),
        hinter: HistoryHinter {},
        colored_prompt: "".to_owned(),
        validator: MatchingBracketValidator::new(),
    };

    let mut editor = Editor::with_history(config, MemHistory::new())?;
    editor.set_helper(Some(h));

    Ok(editor)
}