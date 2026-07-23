//! Semantic command blocks.
//!
//! Shell integration (OSC 133 / OSC 633, emitted by `shell/mimi.fish`) marks
//! prompt/command/output boundaries in the byte stream. mimi turns every
//! executed command into a `Block`: command line, cwd, captured output, exit
//! code, timing. Agents consume these over the control socket instead of
//! scraping the screen.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

/// Cap on captured output per block. Output keeps flowing to the screen
/// regardless; only the agent-visible capture is truncated.
pub const OUTPUT_CAP: usize = 1024 * 1024;
const MAX_BLOCKS: usize = 512;

#[derive(Clone, Debug)]
pub struct Block {
    pub id: u64,
    pub command: String,
    pub cwd: Option<String>,
    pub output: String,
    pub output_truncated: bool,
    pub exit: Option<i32>,
    pub running: bool,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
}

#[derive(Default)]
pub struct BlockStore {
    blocks: VecDeque<Block>,
    next_id: u64,
    /// Command line reported by OSC 633;E just before execution.
    pending_command: Option<String>,
    /// Index (in `blocks`) of the currently running block.
    running: Option<usize>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl BlockStore {
    pub fn set_pending_command(&mut self, cmd: String) {
        self.pending_command = Some(cmd);
    }

    /// OSC 133;C — the shell is about to execute a command.
    pub fn start(&mut self, cwd: Option<String>) -> u64 {
        // A new command implicitly ends any block that never saw its D mark.
        self.finish_running(None);
        let id = self.next_id;
        self.next_id += 1;
        self.blocks.push_back(Block {
            id,
            command: self.pending_command.take().unwrap_or_default(),
            cwd,
            output: String::new(),
            output_truncated: false,
            exit: None,
            running: true,
            started_ms: now_ms(),
            ended_ms: None,
        });
        if self.blocks.len() > MAX_BLOCKS {
            self.blocks.pop_front();
        }
        self.running = Some(self.blocks.len() - 1);
        id
    }

    /// OSC 133;D;<exit> — the command finished. Returns the block id.
    pub fn finish(&mut self, exit: Option<i32>) -> Option<u64> {
        self.finish_running(exit)
    }

    fn finish_running(&mut self, exit: Option<i32>) -> Option<u64> {
        let idx = self.running.take()?;
        let block = self.blocks.get_mut(idx)?;
        block.running = false;
        block.exit = exit;
        block.ended_ms = Some(now_ms());
        Some(block.id)
    }

    /// Append printed output to the running block (called from the print/
    /// linefeed paths; the caller skips alt-screen output).
    #[inline]
    pub fn capture(&mut self, ch: char) {
        if let Some(idx) = self.running {
            let block = &mut self.blocks[idx];
            if block.output.len() < OUTPUT_CAP {
                block.output.push(ch);
            } else {
                block.output_truncated = true;
            }
        }
    }

    #[inline]
    pub fn is_capturing(&self) -> bool {
        self.running.is_some()
    }

    /// The id the next started block will get — lets pollers detect "a block
    /// newer than now" without racing.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    pub fn get(&self, id: u64) -> Option<&Block> {
        self.blocks.iter().find(|b| b.id == id)
    }

    pub fn last(&self) -> Option<&Block> {
        self.blocks.back()
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Block> {
        self.blocks.iter()
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}
