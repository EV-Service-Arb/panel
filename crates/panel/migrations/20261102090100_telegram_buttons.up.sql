-- A message's buttons are what sites described: what each says, what pressing it calls, what the
-- message then says and which buttons it leaves. Sent messages keep theirs, spelled out from the
-- lead they named; what was still queued is given up, its sealed text being of the earlier shape.
UPDATE telegram_outbox SET state = 'dead', text_sealed = NULL, data_key_fp = NULL, leased_until = NULL, last_error = 'cut-over'
	WHERE state = 'pending';
UPDATE telegram_outbox SET buttons = (
	SELECT json_group_array(json(CASE b.value
		WHEN 'take' THEN json_object('id', 'take', 'label', 'Взял', 'pressed', 'Взял',
			'call', json_object('method', 'POST', 'path', '/api/v1/leads/' || brand_id || '/' || lead_id || '/stage', 'body', json_object('stage', 'contacted')),
			'leaves', json_array())
		WHEN 'no_answer' THEN json_object('id', 'no_answer', 'label', 'Не дозвонился', 'pressed', 'Не дозвонился',
			'call', json_object('method', 'POST', 'path', '/api/v1/leads/' || brand_id || '/' || lead_id || '/no_answer', 'body', json('null')),
			'leaves', json_array('take'))
	END))
	FROM json_each(telegram_outbox.buttons) b
) WHERE json_array_length(buttons) > 0;
ALTER TABLE telegram_outbox DROP COLUMN brand_id;
ALTER TABLE telegram_outbox DROP COLUMN lead_id;
-- The last of sites' notes already in the outbox.
ALTER TABLE telegram_poller ADD COLUMN notes_after INTEGER NOT NULL DEFAULT 0;
