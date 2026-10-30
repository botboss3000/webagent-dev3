//! Thin reconstruction for existing foreign native handler signatures.
//! Current Engine scope/catalog/actor/binding always replace captured root defaults.
use crate::*;
use webagent_engine::runtime::native::{Call, NativeDispatch, View};
impl
    NativeDispatch<
        Store,
        loop_runtime::NativeSchemas,
        abilities::Catalog,
        shared_chat::SessionAccess,
    > for super::policy::NativePolicy
{
    fn call<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        request: Call,
    ) -> impl std::future::Future<Output = Result<Value>> + Send + 'a {
        async move {
            let app = self.host.app(view);
            let Call {
                sid,
                run_id,
                generation,
                name,
                args,
                pending_id,
                policy_version,
                cancel,
            } = request;

            #[cfg(test)]
            if name == "test_owner_purchase" {
                let run = app.run(&run_id).await?;
                return Ok(
                    json!({"status":"success","output":{"approved_for":run["requester_user_id"]}}),
                );
            }
            // Low-level native reads also have internal callers without a stored run.
            // Foreground dispatch above still requires its persisted authorization.
            let optimization_run = app
                .store
                .get(&key("run", &run_id))
                .await?
                .unwrap_or(Value::Null);
            if !crate::optimization_prompt::allows_tool(&optimization_run, &name, &args) {
                return Ok(
                    json!({"status":"denied","output":"Optimization is restricted to isolated resource experiments and cannot change prompts, skills, settings or deploy a candidate"}),
                );
            }
            if !crate::optimization_prompt::bind_task(&app, &run_id, &optimization_run, &args)
                .await?
            {
                return Ok(
                    json!({"status":"denied","output":"An explicit optimization run must reuse its single isolated task; changing task IDs cannot reset experiment budgets"}),
                );
            }
            if optimization_run["plugin_bindings"].get(&name).is_some() {
                return crate::ability_plugins::execute(
                    &app,
                    &sid,
                    &name,
                    &args,
                    &pending_id,
                    &optimization_run["plugin_bindings"][&name],
                    cancel,
                )
                .await;
            }
            let native_call = tools::native::Call {
                sid: &sid,
                run_id: &run_id,
                generation: &generation,
                pending_id: &pending_id,
            };
            if tools::native::visual_capture::owns(&name) {
                tools::native::visual_capture::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::vault::owns(&name) {
                tools::native::vault::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::visualizer::owns(&name) {
                tools::native::visualizer::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::front_desk::owns(&name) {
                // Boxed so this branch does not grow the shared dispatch future's stack frame.
                Box::pin(tools::native::front_desk::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                ))
                .await
            } else if tools::native::knowledge_files::owns(&name) {
                // Boxed so this branch does not grow the shared dispatch future's stack frame.
                Box::pin(tools::native::knowledge_files::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                ))
                .await
            } else if tools::native::webhooks::owns(&name) {
                tools::native::webhooks::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::base_extras::owns(&name) {
                tools::native::base_extras::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::orchestration::owns(&name) {
                tools::native::orchestration::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::functions::owns(&name) {
                tools::native::functions::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::ui_admin::owns(&name) {
                tools::native::ui_admin::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::app_control::owns(&name) {
                tools::native::app_control::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::core_image_generation::owns(&name) {
                tools::core_image_generation::execute(
                    &app,
                    &sid,
                    &run_id,
                    &generation,
                    &pending_id,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::image_vision::owns(&name) {
                tools::native::image_vision::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &args,
                    cancel,
                )
                .await
            } else if tools::core_agent_management::owns(&name) {
                tools::core_agent_management::execute(
                    &app,
                    &sid,
                    &run_id,
                    &generation,
                    &pending_id,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::model_switcher::owns(&name) {
                tools::native::model_switcher::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::user_files::owns(&name) {
                tools::native::user_files::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if crate::session_messaging::owns(&name) {
                crate::session_messaging::execute(
                    &app,
                    &sid,
                    &run_id,
                    &generation,
                    &pending_id,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if name == "promotion_propose" {
                propose_promotion(
                    &app,
                    &sid,
                    &pending_id,
                    &run_id,
                    &args,
                    policy_version,
                    cancel,
                )
                .await
            } else if matches!(name.as_str(), "prompt_slot_read" | "prompt_slot_write") {
                crate::api::sessions::execute_prompt_slot_receipted(
                    &app,
                    &name,
                    &args,
                    Some(&pending_id),
                )
                .await
            } else if name == "state_inspect" {
                match serde_json::from_value::<InspectArgs>(args) {
                    Ok(query) if validate_inspect_args(&query).is_ok() => {
                        inspect_state(&app, &sid, &query)
                            .await
                            .map(|output| json!({"status":"success","output":output}))
                    }
                    _ => {
                        Ok(json!({"status":"denied","output":"Invalid state inspection arguments"}))
                    }
                }
            } else if tools::native::mode::owns(&name) {
                tools::native::mode::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if crate::mcp::owns(&name) {
                crate::mcp::execute(&app, &sid, &name, &args, cancel).await
            } else if crate::main_chat::owns(&name) {
                crate::main_chat::tool(&app, &sid, &run_id, &name, &args).await
            } else if crate::modules::owns_tool(&name) {
                crate::modules::tool_call(&app, &sid, &run_id, &name, &args, &pending_id, cancel)
                    .await
            } else if matches!(
                name.as_str(),
                "workspace_seed"
                    | "source_review"
                    | "source_patch"
                    | "v3_workspace_status"
                    | "v3_cargo_task"
            ) {
                tools::administrator::workspace_host(&app, &sid, &name, &args, None, cancel).await
            } else if tools::administrator::owns(&name) {
                tools::administrator::execute(
                    &app,
                    &sid,
                    &run_id,
                    &name,
                    &args,
                    &pending_id,
                    policy_version,
                    cancel,
                )
                .await
            } else if tools::native::task::owns(&name) {
                tools::native::task::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::self_edit::owns(&name) {
                tools::native::self_edit::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::tool_search::owns(&name) {
                tools::native::tool_search::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::search::owns(&name) {
                tools::native::search::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::compaction::owns(&name) {
                tools::native::compaction::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::recall::owns(&name) {
                tools::native::recall::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::prompt::owns(&name) {
                tools::native::prompt::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::context_assets::owns(&name) {
                tools::native::context_assets::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::core_skills::owns(&name) {
                // Skill reads recheck current caller, agent and bundle validity; no activation.
                tools::core_skills::execute(&app, &sid, &run_id, &name, &args, cancel).await
            } else if tools::native::session::owns(&name) {
                // The authenticated runtime supplies the session; arguments cannot choose one.
                tools::native::session::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::utilities::owns(&name) {
                // Native utilities remain behind the same live Rust authorization as worker calls.
                tools::native::utilities::execute(&name, &args, cancel).await
            } else if tools::web_search::owns(&name) {
                tools::web_search::execute(
                    &app,
                    &sid,
                    &run_id,
                    &generation,
                    &pending_id,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::web_access::owns(&name) {
                tools::native::web_access::execute(&name, &args, cancel).await
            } else if tools::native::decision::owns(&name) {
                // The compiled adapter resolves this session's assigned Decision model and credential.
                tools::native::decision::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if tools::native::directory_decision::owns(&name) {
                // Rust checks file_list authority before the worker returns names to Jev.
                tools::native::directory_decision::execute(
                    &tools::native_host::Host(&app),
                    native_call,
                    &name,
                    &args,
                    cancel,
                )
                .await
            } else if matches!(name.as_str(), "shell" | "python")
                && optimization_run["asset_revision"].is_string()
            {
                tools::execute_trusted(
                    &name,
                    &args,
                    &sid,
                    &app.tool_socket,
                    Some(json!({"codebase_revision":optimization_run["asset_revision"]})),
                    cancel,
                )
                .await
            } else {
                tools::execute(&name, &args, &sid, &app.tool_socket, cancel).await
            }
        }
    }
    #[cfg(test)]
    fn test_purchase<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        run: &'a Value,
        action: &'a Value,
        args: &'a Value,
    ) -> impl std::future::Future<Output = Result<Value>> + Send + 'a {
        async move { crate::actions::test_purchase(&self.host.app(view), run, action, args).await }
    }

    fn decision<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        sid: &'a str,
        run: &'a str,
        args: &'a Value,
        cancel: CancellationToken,
    ) -> impl std::future::Future<Output = Result<Value>> + Send + 'a {
        async move {
            let app = self.host.app(view);
            crate::tools::native::decision::decide(
                &crate::tools::native_host::Host(&app),
                sid,
                run,
                args,
                cancel,
            )
            .await
        }
    }
    fn own_skill_guidance<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        sid: &'a str,
        run: &'a str,
        read_only: bool,
    ) -> impl std::future::Future<Output = Result<Vec<webagent_capability_contracts::Guidance>>>
           + Send
           + 'a {
        async move {
            crate::tools::core_skills::own_capability_guidance(
                &self.host.app(view),
                sid,
                run,
                read_only,
            )
            .await
        }
    }

    fn optimization_prepare<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        sid: &'a str,
        run: &'a mut Value,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { crate::optimization_prompt::prepare(&self.host.app(view), sid, run).await }
    }
    fn optimization_guidance(run: &Value) -> &str {
        crate::optimization_prompt::guidance(run)
    }
    fn chat_model_preference<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        run: &'a mut Value,
    ) -> impl std::future::Future<Output = Result<bool>> + Send + 'a {
        async move {
            let app = self.host.app(view);
            let changed = crate::apply_chat_model_preference(&app, run).await?;
            Ok(
                tools::native::model_switcher::apply(&tools::native_host::Host(&app), run).await?
                    || changed,
            )
        }
    }
    fn messaging_preflight<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        run: &'a mut Value,
        name: &'a str,
        args: &'a Value,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move {
            let app = self.host.app(view);
            tools::native::model_switcher::require_premium_confirmation(
                &tools::native_host::Host(&app),
                run,
                name,
                args,
            )
            .await?;
            crate::session_messaging::preflight(&app, run, name, args).await
        }
    }

    fn host_fingerprint<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        sid: &'a str,
        run: &'a str,
        authority: &'a str,
    ) -> impl std::future::Future<Output = Result<String>> + Send + 'a {
        async move {
            crate::tools::administrator::host_fingerprint(&self.host.app(view), sid, run, authority)
                .await
        }
    }
    fn recovery_build_socket(
        &self,
        view: View<
            '_,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
    ) -> PathBuf {
        crate::tools::administrator::build_socket(&self.host.app(view))
    }

    fn naming_service<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
    ) -> impl std::future::Future<Output = ()> + Send + 'a {
        async move { crate::session_naming::service(self.host.app(view)).await }
    }
    fn manager_service<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
    ) -> impl std::future::Future<Output = ()> + Send + 'a {
        async move { crate::manager::scheduler(self.host.app(view)).await }
    }
    fn modules_service<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
    ) -> impl std::future::Future<Output = ()> + Send + 'a {
        async move { crate::modules::service(self.host.app(view)).await }
    }
    fn followup_tick<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { crate::followup_queue::scheduler_tick(&self.host.app(view)).await }
    }

    fn code_index_configuration_attach<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        args: &'a Value,
        result: &'a mut Value,
        cancel: &'a CancellationToken,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move {
            let app = self.host.app(view);
            webagent_administration_host::code_index::attach_configuration(
                &crate::tools::administrator::Host(&app),
                args,
                result,
                cancel,
            )
            .await
        }
    }
    fn code_index_configuration_snippet_bytes(source: &Value) -> usize {
        webagent_administration::code_index::snippet_bytes(source)
    }

    fn messaging_revalidate<'a>(
        &'a self,
        view: View<
            'a,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
        sid: &'a str,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { crate::session_messaging::revalidate(&self.host.app(view), sid).await }
    }
}
