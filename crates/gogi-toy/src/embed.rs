//! The toy embedding model: the tokens of an input with its task prompt, and its vector.
//!
//! An input renders as `<bos>`, the task's prompt as bytes, then the content as a message's
//! content renders. The vector is the mean, over each token position, of the first 64 logits of
//! the model state after that position, each divided by 6 so it lies in -1 to 1. A cut to `d`
//! dimensions takes the first `d` components of that mean. The vector is then normalized to an
//! L2 norm of 1 in float64 and written as float32. The same seed and input give the same bits,
//! and an input's vector does not depend on any other input.

use gogi_proto::message::Content;

use crate::{BOS, State, push_content, push_text};

/// The template id of the toy embedding model, which names its task prompts.
pub const TEMPLATE: &str = "gogi-toy-embed-1";

/// Each task and its prompt, in the shape of EmbeddingGemma's.
pub const TASKS: [(&str, &str); 3] = [("query", "task: search result | query: "), ("document", "title: none | text: "), ("similarity", "task: sentence similarity | query: ")];

/// The vector lengths the toy offers, largest first.
pub const DIMENSIONS: [u32; 3] = [64, 32, 16];

/// The prompt of task `task`, or `None` when the toy has no such task.
pub fn prompt(task: &str) -> Option<&'static str> {
    TASKS.iter().find(|(t, _)| *t == task).map(|(_, p)| *p)
}

/// The tokens of an input as the toy reads it: `<bos>`, the prompt of `task`, and the content.
pub fn tokens(task: &str, input: &Content) -> Vec<u32> {
    let mut t = vec![BOS];
    push_text(&mut t, prompt(task).unwrap_or_else(|| panic!("the toy has no task {:?}; check the task first", task)));
    push_content(&mut t, Some(input));
    t
}

/// The vector of `tokens` for served model `seed`, cut to `d` dimensions.
pub fn vector(seed: u64, tokens: &[u32], d: usize) -> Vec<f32> {
    assert!(d <= DIMENSIONS[0] as usize && !tokens.is_empty(), "a toy vector has 1 to {} dimensions and one token or more", DIMENSIONS[0]);
    let mut sum = vec![0.0f64; d];
    let mut state = State::start(seed);
    for &t in tokens {
        state = state.step(t);
        for (s, l) in sum.iter_mut().zip(state.logits()) {
            *s += l / 6.0;
        }
    }
    let mean: Vec<f64> = sum.iter().map(|s| s / tokens.len() as f64).collect();
    let norm = mean.iter().map(|x| x * x).sum::<f64>().sqrt();
    mean.iter().map(|x| (x / norm) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_input_reads_as_bos_the_task_prompt_and_its_text() {
        let t = tokens("query", &"hi".into());
        assert_eq!(t[0], BOS);
        assert_eq!(t.len(), 1 + "task: search result | query: ".len() + 2);
        assert_eq!(&t[t.len() - 2..], &[u32::from(b'h'), u32::from(b'i')]);
    }

    #[test]
    fn a_vector_is_a_unit_vector_that_replays_and_depends_on_task_and_seed() {
        let q = vector(0, &tokens("query", &"pin a snapshot".into()), 64);
        assert_eq!(q.len(), 64);
        let n: f64 = q.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
        assert!((n - 1.0).abs() < 1e-6, "{n}");
        assert_eq!(q, vector(0, &tokens("query", &"pin a snapshot".into()), 64));
        assert_ne!(q, vector(0, &tokens("document", &"pin a snapshot".into()), 64));
        assert_ne!(q, vector(1, &tokens("query", &"pin a snapshot".into()), 64));
    }

    #[test]
    fn a_cut_is_the_whole_vector_cut_and_normalized_again() {
        let t = tokens("document", &"a chunk of a file".into());
        let whole = vector(3, &t, 64);
        let want = gogi_proto::embeddings::cut(&whole, 16);
        let got = vector(3, &t, 16);
        assert!(got.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-6), "{got:?} {want:?}");
    }
}
