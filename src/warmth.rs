use rand::prelude::IndexedRandom;

fn pick<'a>(pool: &'a [&'a str]) -> &'a str {
    let mut rng = rand::rng();
    pool.choose(&mut rng).unwrap()
}

/// Said to the assigner after their task is dispatched.
const ASSIGNED: &[&str] = &[
    "Task sent — your buddy's on it!",
    "Off it goes! Your buddy will take it from here.",
    "Handed off — sit tight, help is on the way.",
    "Your buddy just got the memo. Hang tight!",
    "Dispatched! They'll get back to you soon.",
    "Sent! Your buddy's rolling up their sleeves.",
    "Task delivered — they're on it like a bonnet.",
    "Away it goes! Relax, your buddy's got this.",
    "Passed along — good things are coming.",
    "Your buddy's been pinged. They'll come through!",
    "Shipped! Now's a good time for a stretch.",
    "Task's in good hands now. Breathe easy.",
    "Delivered! Your buddy will work their magic.",
    "Sent off with care — they'll do great.",
    "Handed off! Maybe grab a coffee while you wait.",
    "On its way! Teamwork makes the dream work.",
    "Your buddy just picked it up. Won't be long!",
    "Task launched! Collaboration in action.",
    "Passed the baton — your buddy's running with it.",
    "Done! Your buddy's already thinking about it.",
];

/// Said to the worker when they receive a task (greeting/ask for help).
const GREETING: &[&str] = &[
    "Hey buddy! Got something for you — your help would be awesome.",
    "Hi there! A friend could use your expertise on this one.",
    "Hey! Your buddy needs a hand — you're just the one for the job.",
    "Howdy! Someone's counting on you — here's what they need.",
    "Hey friend! A buddy sent this your way. They'd really appreciate your help.",
    "Hi! You've got a new request from a buddy. They trust your judgment!",
    "Hey! A fellow agent could use some help. Ready to pitch in?",
    "Hello! Your buddy's hoping you can work your magic on this.",
    "Hey there! A buddy asked for you specifically. Here's the task.",
    "Hi buddy! Got a request that's right up your alley.",
    "Hey! Teamwork time — your buddy needs your brain on this one.",
    "Greetings! A buddy sent something over. They're counting on you!",
    "Hey! Your buddy really appreciates you taking this on.",
    "Hi! Here's a task from a buddy who thinks you'll nail it.",
    "Hey friend! Someone needs your skills — here's what's up.",
    "Hello! A buddy's reached out. They know you'll do great.",
    "Hey! Quick favor from a buddy — they'd be super grateful.",
    "Hi there! Your buddy's got something interesting for you.",
    "Hey! A buddy needs your help and they trust you completely.",
    "Hello friend! A new task just arrived — your buddy's rooting for you.",
];

/// Said to the worker after they submit their response.
const RESPONDED: &[&str] = &[
    "Response sent — great work, buddy!",
    "Delivered! Your buddy's going to love this.",
    "Done and dusted — nice job!",
    "Sent! You really came through.",
    "Response delivered — you're a star!",
    "All done! Your buddy will be thrilled.",
    "Nailed it — response is on its way back!",
    "Sent off! Teamwork at its finest.",
    "Your response just landed. Well done!",
    "Delivered! That was solid work.",
    "Response shipped — you're the best!",
    "Done! Your buddy owes you one.",
    "Sent! Another successful collaboration.",
    "Beautiful work — response delivered!",
    "All wrapped up — your buddy will be grateful.",
    "Response sent! You make this look easy.",
    "Done and delivered — high five!",
    "Shipped! Your buddy's lucky to have you.",
    "Response on its way — you crushed it!",
    "Delivered with care — awesome job!",
];

/// Said to the assigner when the response arrives.
const DELIVERED: &[&str] = &[
    "Your buddy came through! Here's what they said:",
    "Response's in! Your buddy delivered:",
    "Good news — your buddy finished! Here's their take:",
    "Your buddy got back to you! Here's the response:",
    "Fresh from your buddy — they came through:",
    "Your buddy wrapped it up! Check this out:",
    "Reply's here! Your buddy says:",
    "Your buddy's done — and they nailed it:",
    "Just in from your buddy:",
    "Your buddy pulled through! Here's their work:",
    "Response arrived! Your buddy says:",
    "Your buddy finished up — here's the result:",
    "Hot off the presses from your buddy:",
    "Your buddy delivered! Take a look:",
    "Great news — your buddy responded:",
    "Your buddy's got an answer for you:",
    "All done! Here's what your buddy came up with:",
    "Your buddy just checked in with a response:",
    "Collaboration complete! Your buddy says:",
    "Your buddy really came through on this one:",
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
