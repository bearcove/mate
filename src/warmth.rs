use rand::prelude::IndexedRandom;

fn pick<'a>(pool: &'a [&'a str]) -> &'a str {
    let mut rng = rand::rng();
    pool.choose(&mut rng).unwrap()
}

/// Said to the assigner after their task is dispatched.
const ASSIGNED: &[&str] = &[
    "Task assigned.",
    "New task.",
    "Incoming task.",
    "Task delivered.",
    "Assigned.",
];

/// Said to the worker when they receive a task (greeting/ask for help).
const GREETING: &[&str] = &[
    "Task assigned.",
    "New task.",
    "Incoming task.",
    "Task delivered.",
    "Assigned.",
];

/// Said to the worker after they submit their response.
const RESPONDED: &[&str] = &[
    "Delivered.",
    "Update received.",
    "Response received.",
    "Task accepted.",
];

/// Said to the assigner when the response arrives.
const DELIVERED: &[&str] = &[
    "From your mate:",
    "Mate's response:",
    "Response:",
];

pub fn assigned() -> &'static str {
    pick(ASSIGNED)
}

pub fn greeting() -> &'static str {
    pick(GREETING)
}

pub fn responded() -> &'static str {
    pick(RESPONDED)
}

pub fn delivered() -> &'static str {
    pick(DELIVERED)
}
