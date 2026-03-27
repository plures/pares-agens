//! Integration test: store → recall → verify relevance
//!
//! Validates the full PluresLM pipeline end-to-end without mocking internals.

use pares_agens_core::memory::{
    embed::MockEmbedder,
    entry::{Exchange, MemoryCategory},
    store::InMemoryStore,
    PluresLm,
};
use std::sync::Arc;

fn make_lm() -> PluresLm {
    PluresLm::new(
        Arc::new(InMemoryStore::new()),
        Box::new(MockEmbedder),
        128_000,
    )
}

/// Core acceptance criterion: store memories, recall by query, verify ordering.
#[tokio::test]
async fn store_recall_verify_relevance() {
    let lm = make_lm();

    // Store a Rust-async memory
    let rust_exchange = Exchange {
        user: "How do I use async await in Rust programming?".into(),
        assistant: "Use `async fn` and `.await` on futures. Add tokio runtime to Cargo.toml for \
                    the executor. Mark functions with async keyword."
            .into(),
    };
    let rust_ids = lm.capture(&rust_exchange).await.unwrap();
    assert_eq!(
        rust_ids.len(),
        1,
        "should store one memory for the rust exchange"
    );

    // Store an unrelated memory about cooking
    let food_exchange = Exchange {
        user: "How do I make the best espresso coffee at home?".into(),
        assistant: "Grind fresh beans finely, use 200 degrees Fahrenheit water, pull a 25 second \
                    shot with 9 bar pressure through a portafilter."
            .into(),
    };
    let food_ids = lm.capture(&food_exchange).await.unwrap();
    assert_eq!(
        food_ids.len(),
        1,
        "should store one memory for the food exchange"
    );

    // Store a Python memory to add more signal
    let python_exchange = Exchange {
        user: "What is the GIL in Python and how does it affect threading?".into(),
        assistant: "The Global Interpreter Lock prevents true parallel Python thread execution. \
                    Use multiprocessing or async/await with asyncio for concurrency instead."
            .into(),
    };
    lm.capture(&python_exchange).await.unwrap();

    // Recall with a Rust-async query — the Rust memory should rank highest
    let recalled = lm
        .recall("async await Rust futures tokio", 10, &[])
        .await
        .unwrap();

    assert!(
        !recalled.is_empty(),
        "recall must return at least one memory"
    );
    assert!(
        recalled[0].score > 0.0,
        "top result must have a positive relevance score"
    );

    // The Rust memory should rank above the food memory
    let rust_pos = recalled
        .iter()
        .position(|m| m.content.contains("tokio"))
        .expect("Rust memory must appear in results");
    let food_pos = recalled
        .iter()
        .position(|m| m.content.contains("espresso") || m.content.contains("coffee"));

    if let Some(food_idx) = food_pos {
        assert!(
            rust_pos < food_idx,
            "Rust memory (pos {rust_pos}) must rank above food memory (pos {food_idx})"
        );
    }

    // Scores should be in descending order
    for window in recalled.windows(2) {
        assert!(
            window[0].score >= window[1].score,
            "results must be sorted by descending score: {} < {}",
            window[0].score,
            window[1].score
        );
    }
}

/// Category exclusion filters out unwanted categories.
#[tokio::test]
async fn category_exclusion_works() {
    let lm = make_lm();

    lm.capture(&Exchange {
        user: "I prefer snake_case for variable naming conventions always.".into(),
        assistant: "Agreed — snake_case is the standard Rust naming convention for \
                    variables and function parameters."
            .into(),
    })
    .await
    .unwrap();

    lm.capture(&Exchange {
        user: "How do I implement the Display trait in Rust?".into(),
        assistant: "Implement fmt::Display by writing impl fmt::Display for YourType and \
                    provide the fmt method that formats the struct fields."
            .into(),
    })
    .await
    .unwrap();

    // Recall with Preference excluded — should only get code-pattern/conversation results
    let results = lm
        .recall("naming convention Rust", 10, &[MemoryCategory::Preference])
        .await
        .unwrap();

    for m in &results {
        assert_ne!(
            m.category,
            MemoryCategory::Preference,
            "Preference memories must be excluded"
        );
    }
}

/// Quality gate: noise and echoes are not stored.
#[tokio::test]
async fn quality_gate_rejects_noise_and_echoes() {
    let lm = make_lm();

    // Noise — too short
    let noise_ids = lm
        .capture(&Exchange {
            user: "ok".into(),
            assistant: "Got it!".into(),
        })
        .await
        .unwrap();
    assert!(noise_ids.is_empty(), "noise must be rejected");

    // Quality exchange
    let good = Exchange {
        user: "What is ownership in Rust and how does borrowing work with lifetimes?".into(),
        assistant: "Ownership ensures memory safety without garbage collection. Each value has \
                    exactly one owner. Borrowing allows references with lifetime annotations."
            .into(),
    };
    let first_ids = lm.capture(&good).await.unwrap();
    assert_eq!(first_ids.len(), 1, "good exchange must be stored");

    // Echo — same exchange again
    let echo_ids = lm.capture(&good).await.unwrap();
    assert!(echo_ids.is_empty(), "echo must be rejected");
}

/// Budget enforcement: inject_context respects the token limit.
#[tokio::test]
async fn inject_context_enforces_budget() {
    let lm = make_lm();

    // Store several memories
    for i in 0..5 {
        lm.capture(&Exchange {
            user: format!("Tell me something interesting about topic number {i} in detail please."),
            assistant: format!(
                "Topic {i} is fascinating because it involves many complex interactions \
                 between different systems and components that all work together."
            ),
        })
        .await
        .unwrap();
    }

    let memories = lm.recall("topic interesting detail", 5, &[]).await.unwrap();
    assert!(!memories.is_empty());

    // 25% of 128 000 = 32 000 tokens → up to 128 000 chars (generous)
    let ctx_default = lm.inject_context(&memories, None);
    assert!(ctx_default.starts_with("# Relevant memories\n\n"));

    // Tight budget: 5 tokens = 20 chars — header alone exceeds this, so no memory
    // items should be included in the output.
    let ctx_tight = lm.inject_context(&memories, Some(5));
    assert!(
        ctx_tight.starts_with("# Relevant memories\n\n"),
        "header must always be present"
    );
    assert!(
        !ctx_tight.contains("1. ["),
        "no memory items should fit within a 5-token budget"
    );
}
