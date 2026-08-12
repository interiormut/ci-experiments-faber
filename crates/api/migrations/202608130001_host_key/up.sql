-- Somewhere to record the host key an SSH host is known by.
--
-- Without this column there are only two ways to connect, and both are wrong:
-- accept whatever key answers, which is a silent machine-in-the-middle on
-- every connection, or refuse every connection. Faber is a multi-user service
-- holding many users' credentials, so the first failure compromises all of
-- them at once, and there is no `known_hosts` to fall back on — a file on the
-- server records the operator's trust decisions, not any user's.
--
-- Null means this host has never been connected to. The first successful
-- connection stores the fingerprint it saw; every connection after verifies
-- against it, and a mismatch is refused rather than trusted.
--
-- Clearing it back to null is deliberate and allowed: a rebuilt machine has a
-- new key, and the operator saying so is exactly the decision this column
-- exists to make explicit rather than automatic.
ALTER TABLE host ADD COLUMN ssh_host_key text;

COMMENT ON COLUMN host.ssh_host_key IS
  'OpenSSH SHA256 fingerprint the host is known by; null until first contact';

-- Same shape as the transport config check above it: a field that means
-- nothing for a local host does not get to hold a value on one.
ALTER TABLE host ADD CONSTRAINT host_ssh_host_key_transport CHECK (
  transport = 'ssh' OR ssh_host_key IS NULL
);
