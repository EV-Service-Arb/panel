UPDATE telegram_outbox SET state = 'dead', text_sealed = NULL, data_key_fp = NULL, leased_until = NULL, last_error = 'cut-over'
	WHERE state = 'pending';
ALTER TABLE telegram_poller DROP COLUMN notes_after;
ALTER TABLE telegram_outbox ADD COLUMN brand_id TEXT;
ALTER TABLE telegram_outbox ADD COLUMN lead_id TEXT;
UPDATE telegram_outbox SET
	brand_id = (SELECT substr(r, 1, instr(r, '/') - 1) FROM (SELECT substr(json_extract(buttons, '$[0].call.path'), length('/api/v1/leads/') + 1) AS r)),
	lead_id = (SELECT substr(t, 1, instr(t, '/') - 1) FROM (SELECT substr(r, instr(r, '/') + 1) AS t FROM (SELECT substr(json_extract(buttons, '$[0].call.path'), length('/api/v1/leads/') + 1) AS r))),
	buttons = (SELECT json_group_array(json_extract(b.value, '$.id')) FROM json_each(telegram_outbox.buttons) b)
	WHERE json_array_length(buttons) > 0;
