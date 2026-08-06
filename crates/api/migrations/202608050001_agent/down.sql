DROP TRIGGER transcript_append_only ON transcript;
DROP TRIGGER exchange_append_only ON exchange;
DROP TRIGGER run_delete_with_thread ON run;
DROP TRIGGER workspace_membership_policy ON workspace_member;
DROP TRIGGER workspace_owner_only ON workspace;
DROP TRIGGER users_create_workspace ON users;

DROP TABLE spine;
DROP TABLE transcript;
DROP TABLE span;
DROP TABLE exchange;
DROP TABLE blob;
DROP TABLE run;
DROP TABLE thread;
DROP TABLE session_ref;
DROP TABLE session;
DROP TABLE workspace_member;
DROP TABLE workspace;

DROP FUNCTION reject_history_mutation();
DROP FUNCTION protect_run();
DROP FUNCTION enforce_workspace_membership();
DROP FUNCTION protect_user_workspace();
DROP FUNCTION create_user_workspace();
