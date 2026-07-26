-- Migration 12: add serial-port configuration columns to `hosts`.
--
-- These only matter for `HostKind::Serial` entries; for SSH/Telnet hosts
-- they stay NULL. We use nullable columns (no NOT NULL / no DEFAULT) so
-- existing rows survive without backfill, and the application treats NULL
-- as "use the serial default". Best-effort: on a fresh DB these columns
-- may already exist.
ALTER TABLE hosts ADD COLUMN serial_baud_rate INTEGER;
ALTER TABLE hosts ADD COLUMN serial_data_bits INTEGER;
ALTER TABLE hosts ADD COLUMN serial_parity TEXT;
ALTER TABLE hosts ADD COLUMN serial_stop_bits INTEGER;
ALTER TABLE hosts ADD COLUMN serial_flow_control TEXT;
