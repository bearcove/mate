use facet::Facet;

#[derive(Debug, Clone, Facet)]
pub struct AssignRequest {
    /// The $TMUX_PANE of the requesting agent
    pub source_pane: String,
    /// The tmux session name of the requesting agent
    pub session_name: String,
    /// The task content (read from stdin by the CLI)
    pub content: String,
    /// Optional title for the task
    pub title: Option<String>,
    /// Whether to send /clear to the worker before the task
    pub clear: bool,
    /// Fingerprint of the client binary used to detect upgrades
    pub binary_hash: String,
}

#[derive(Debug, Clone, Facet)]
pub struct RespondRequest {
    pub request_id: String,
    pub session_name: String,
    /// Raw response content from the mate (server will format and deliver)
    pub content: String,
}

#[derive(Debug, Clone, Facet)]
pub struct UpdateRequest {
    pub request_id: String,
    pub session_name: String,
    /// Raw update content from the mate (server will format and deliver)
    pub content: String,
}

#[derive(Debug, Clone, Facet)]
pub struct WaitRequest {
    pub request_id: String,
    pub session_name: String,
    /// How long to block waiting for an event (in seconds)
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Facet)]
pub struct AcceptRequest {
    pub request_id: String,
    pub session_name: String,
}

#[derive(Debug, Clone, Facet)]
pub struct CancelRequest {
    pub request_id: String,
    pub session_name: String,
}

#[derive(Debug, Clone, Facet)]
pub struct SteerRequest {
    pub request_id: String,
    pub session_name: String,
    /// Raw steer content from the captain (server will format and deliver)
    pub content: String,
}

#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum WaitEvent {
    /// A progress update from the mate; more events may follow
    Update { message: String },
    /// A response from the mate; captain should review and accept/steer
    Response { message: String },
    /// Timed out; no event arrived within the window
    Timeout,
}

#[roam::service]
pub trait Coop {
    /// Assign a task to the worker agent. Returns the request ID.
    async fn assign(&self, req: AssignRequest) -> Result<String, String>;
    /// Deliver the final response from mate to captain.
    async fn respond(&self, req: RespondRequest) -> Result<(), String>;
    /// Send a progress update from mate to captain.
    async fn update(&self, req: UpdateRequest) -> Result<(), String>;
    /// Mark a task as accepted and clean up request state.
    async fn accept(&self, req: AcceptRequest) -> Result<(), String>;
    /// Cancel a task and clean up request state.
    async fn cancel(&self, req: CancelRequest) -> Result<(), String>;
    /// Send a steer message from captain to mate.
    async fn steer(&self, req: SteerRequest) -> Result<(), String>;
    /// Block until a progress update or final response arrives (or timeout).
    async fn wait(&self, req: WaitRequest) -> Result<WaitEvent, String>;
}
