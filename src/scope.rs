use std::mem;
use std::thread;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use api::protocol::{Breadcrumb, User, Value};
use client::Client;

use im;

lazy_static! {
    static ref PROCESS_STACK: Mutex<Stack> = Mutex::new(Stack::for_process());
}
thread_local! {
    static THREAD_STACK: RefCell<Stack> = RefCell::new(Stack::for_thread());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StackType {
    Process,
    Thread,
}

#[derive(Debug)]
pub struct Stack {
    layers: Vec<StackLayer>,
    ty: StackType,
}

#[derive(PartialEq, Clone, Copy)]
struct StackLayerToken(*const Stack, usize);

/// Holds contextual data for the current scope.
///
/// The scope is an object that can cloned efficiently and stores data that
/// is locally relevant to an event.  For instance the scope will hold recorded
/// breadcrumbs and similar information.
///
/// The scope can be interacted with in two ways:
///
/// 1. the scope is routinely updated with information by functions such as
///    `add_breadcrumb` which will modify the currently top-most scope.
/// 2. the topmost scope can also be configured through the `configure_scope`
///    method.
///
/// Note that the scope can only be modified but not inspected.  Only the
/// client can use the scope to extract information currently.
#[derive(Default, Debug, Clone)]
pub struct Scope {
    pub(crate) breadcrumbs: im::Vector<Breadcrumb>,
    pub(crate) user: Option<Arc<User>>,
    pub(crate) extra: im::HashMap<String, Value>,
    pub(crate) tags: im::HashMap<String, String>,
}

#[derive(Default, Debug, Clone)]
struct StackLayer {
    client: Option<Arc<Client>>,
    scope: Scope,
}

impl Stack {
    pub fn for_process() -> Stack {
        Stack {
            layers: vec![Default::default()],
            ty: StackType::Process,
        }
    }

    pub fn for_thread() -> Stack {
        Stack {
            layers: vec![
                with_process_stack(|stack| StackLayer {
                    client: stack.client(),
                    scope: stack.scope_mut().clone(),
                }),
            ],
            ty: StackType::Thread,
        }
    }

    pub fn push(&mut self) {
        let scope = self.layers[self.layers.len() - 1].clone();
        self.layers.push(scope);
    }

    pub fn pop(&mut self) {
        if self.layers.len() <= 1 {
            panic!("Pop from empty {:?} stack", self.ty)
        }
        self.layers.pop().unwrap();
    }

    pub fn bind_client(&mut self, client: Arc<Client>) {
        let depth = self.layers.len() - 1;
        self.layers[depth].client = Some(client);
    }

    pub fn client(&self) -> Option<Arc<Client>> {
        self.layers[self.layers.len() - 1].client.clone()
    }

    pub fn scope_mut(&mut self) -> &mut Scope {
        let idx = self.layers.len() - 1;
        &mut self.layers[idx].scope
    }

    fn token(&self) -> StackLayerToken {
        StackLayerToken(self as *const Stack, self.layers.len())
    }
}

fn is_main_thread() -> bool {
    let thread = thread::current();
    let raw_id: u64 = unsafe { mem::transmute(thread.id()) };
    raw_id == 0
}

fn with_process_stack<F, R>(f: F) -> R
where
    F: FnOnce(&mut Stack) -> R,
{
    f(&mut PROCESS_STACK.lock().unwrap())
}

pub fn with_stack<F, R>(f: F) -> R
where
    F: FnOnce(&mut Stack) -> R,
{
    if is_main_thread() {
        with_process_stack(f)
    } else {
        THREAD_STACK.with(|stack| f(&mut *stack.borrow_mut()))
    }
}

/// Crate internal helper for working with clients and scopes.
pub fn with_client_and_scope<F, R>(f: F) -> R
where
    F: FnOnce(Arc<Client>, &mut Scope) -> R,
    R: Default,
{
    with_stack(|stack| {
        if let Some(client) = stack.client() {
            f(client, stack.scope_mut())
        } else {
            Default::default()
        }
    })
}

/// A scope guard.
#[derive(Default)]
pub struct ScopeGuard(Option<StackLayerToken>);

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if let Some(token) = self.0 {
            with_stack(|stack| {
                if stack.token() != token {
                    panic!("Current active stack does not match scope guard");
                } else {
                    stack.pop();
                }
            })
        }
    }
}

/// Pushes a new scope on the stack.
///
/// The currently bound client is propagated to the new scope and already existing
/// data on the scope is inherited.  Modifications done to the inner scope however
/// are isolated from the outer scope.
///
/// This returns a guard.  When the guard is collected the scope is popped again.
///
/// # Example
///
/// ```rust
/// {
///     let _guard = sentry::push_scope();
///     sentry::configure_scope(|scope| {
///         scope.set_tag("some_tag", "some_value");
///     });
///     // until the end of the block the scope is changed.
/// }
/// ```
pub fn push_scope() -> ScopeGuard {
    with_stack(|stack| {
        stack.push();
        ScopeGuard(Some(stack.token()))
    })
}

impl Scope {
    /// Clear the scope.
    ///
    /// By default a scope will inherit all values from the higher scope.
    /// In some situations this might not be what a user wants.  Calling
    /// this method will wipe all data contained within.
    pub fn clear(&mut self) {
        *self = Default::default();
    }

    /// Sets the user for the current scope.
    pub fn set_user(&mut self, user: Option<User>) {
        self.user = user.map(Arc::new);
    }

    /// Sets a tag to a specific value.
    pub fn set_tag<V: ToString>(&mut self, key: &str, value: V) {
        self.tags = self.tags.insert(key.to_string(), value.to_string());
    }

    /// Removes a tag.
    pub fn remove_tag(&mut self, key: &str) {
        // annoyingly this needs a String :(
        self.tags = self.tags.remove(&key.to_string());
    }

    /// Sets a extra to a specific value.
    pub fn set_extra(&mut self, key: &str, value: Value) {
        self.extra = self.extra.insert(key.to_string(), value);
    }

    /// Removes a extra.
    pub fn remove_extra(&mut self, key: &str) {
        // annoyingly this needs a String :(
        self.extra = self.extra.remove(&key.to_string());
    }
}