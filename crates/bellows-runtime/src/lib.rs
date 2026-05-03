//! `bellows-runtime` — the workflow engine.
//!
//! v0.1 ships an [`Engine`] type that drives a [`Workflow`] end-to-end:
//!
//! 1. Load or create a [`Session`].
//! 2. Build a [`StepCtx`] with model + tools + sandbox + skills.
//! 3. Call `Workflow::execute(ctx, input)`.
//! 4. Persist the resulting session.
//!
//! The autonomous *step loop* (model → tool calls → observations → repeat)
//! lives in this crate — `bellows-core` only declares the contract. v0.1
//! provides the loop scaffolding; full streaming + parallel tool execution
//! lands in v0.2.

use std::sync::Arc;

use bellows_core::{
    ModelProvider, Result, Sandbox, Session, SessionStore, SkillSet, StepCtx, ToolRegistry,
    Workflow,
};
use bellows_tool::SimpleRegistry;
use tracing::{Span, info_span};

/// One configured runtime instance for a specific workflow.
pub struct Engine<W: Workflow> {
    workflow: Arc<W>,
    store: Arc<dyn SessionStore>,
}

impl<W: Workflow> Engine<W> {
    /// Construct an engine with a custom session store.
    #[must_use]
    pub fn new(workflow: W, store: Arc<dyn SessionStore>) -> Self {
        Self {
            workflow: Arc::new(workflow),
            store,
        }
    }

    /// Run the workflow once. Loads or creates the session as needed.
    pub async fn run(&self, input: W::Input) -> Result<W::Output> {
        let mut session = Session::new();
        let span = info_span!("workflow.run", name = self.workflow.name(), session = %session.id);
        let model = self.workflow.model();
        let sandbox = self.workflow.sandbox();
        let tools_vec = self.workflow.tools();
        let mut registry = SimpleRegistry::new();
        for t in tools_vec {
            registry.register(t);
        }
        let tools: Arc<dyn ToolRegistry> = Arc::new(registry);

        let result = self
            .invoke(&mut session, model, tools, sandbox, &span, input)
            .await?;

        self.store.save(&session).await?;
        Ok(result)
    }

    async fn invoke(
        &self,
        session: &mut Session,
        model: Arc<dyn ModelProvider>,
        tools: Arc<dyn ToolRegistry>,
        sandbox: Arc<dyn Sandbox>,
        span: &Span,
        input: W::Input,
    ) -> Result<W::Output> {
        let skills: &dyn SkillSet = self.workflow.skills();
        let mut ctx = StepCtx {
            session,
            model,
            tools,
            sandbox,
            skills,
            trace: span.clone(),
        };
        self.workflow.execute(&mut ctx, input).await
    }
}
