-- Automatic focused-review addressing is a project setting now, not a session
-- permission mode, so sessions that selected it keep editing as Auto Edit.
UPDATE session
SET permission_mode = 'auto_edit'
WHERE permission_mode = 'auto_edit_address_comments';
