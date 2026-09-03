operation에서 현재 project나 config 등에도 접근해야할 수도 있을 것 같아.
core를 runtimedeps에 집어넣자니 너무 cycle이 생기는 것 같고 그것조차 sessionruntimedeps에 각각 따로 집어넣기도 애매한 것 같아.

그리고 예를 들어 plugin에서 command로 새 세션을 만들거나 기존 세션을 복구하는 기능이 생길 것 같은데 그 경우는 어떻게 처리해야할까?

너무 설계를 복잡하게 하지 말고 최대한 미니멀하고 우아한 설계를 만들어

====

Core를 얇은 Arc handle로 만듭니다.

#[derive(Clone)]
pub struct Core {
    inner: Arc<CoreInner>,
}

struct CoreInner {
    registry: Registry,
    config: Config,
    project: Option<Project>,
}

그리고:

pub struct SessionRuntime {
    core: Core,

    session_id: SessionId,
    group_id: SessionGroupId,
    workdir: Arc<dyn Workdir>,

    sender: mpsc::Sender<SessionRequest>,
    snapshot: watch::Receiver<SessionState>,
    events: broadcast::Sender<RuntimeEvent>,
}

즉, SessionRuntimeDeps는 아예 제거하는 쪽입니다.

impl SessionRuntime {
    pub fn spawn(
        state: SessionState,
        core: Core,
    ) -> Result<Arc<Self>> {
        let project = core.project()?;

        let base = Arc::new(NativeWorkdir::new(&project.root)?);

        let workdir = core.registry().layer_workdir(
            &WorkdirLayerContext {
                project_id: project.id,
                project_name: project.name.clone(),
                session_group_id: state.group_id,
            },
            base,
        );

        let store = core.registry().session_store();

        // actor spawn ...

        Ok(Arc::new(Self {
            core,
            session_id: state.session_id,
            group_id: state.group_id,
            workdir,
            // ...
        }))
    }
}

impl Operations {
    pub fn project(&self) -> Result<Project> {
        self.runtime()?.core().project()
    }

    pub fn config(&self) -> Result<Config> {
        Ok(self.runtime()?.core().config().clone())
    }

    pub fn project_id(&self) -> Result<ProjectId> {
        Ok(self.project()?.id)
    }

    pub(crate) fn registry(&self) -> Result<Registry> {
        Ok(self.runtime()?.core().registry())
    }

    pub(crate) fn workdir(&self) -> Result<Arc<dyn Workdir>> {
        Ok(self.runtime()?.workdir())
    }
}


현재 ToolContext/CommandContext에는 다음 값들이 반복해서 들어갑니다.

project_id
session_group_id
session_id
operations
workdir

앞서 구조로 가면 이 중 대부분은 operations에서 이미 얻을 수 있습니다. 따라서 장기적으로는:

pub struct ToolContext {
    pub turn_id: TurnId,
    pub operations: Operations,
    pub cancellation: CancellationToken,
}

pub struct CommandContext {
    pub operations: Operations,
    pub cancellation: CancellationToken,
}

정도로 줄여도 됩니다.

## SessionRuntime 이름 변경
-> SessionHost
