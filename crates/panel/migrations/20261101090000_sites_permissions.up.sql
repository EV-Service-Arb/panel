-- The sites business's permissions moved under `sa:sites:`; what concierge confirmed and what was
-- asked for is re-spelled, not re-asked.
UPDATE telegram_links SET permissions = replace(replace(permissions, '"sa:work:', '"sa:sites:work:'), '"sa:analysis:', '"sa:sites:analysis:')
	WHERE permissions IS NOT NULL;
UPDATE telegram_link_tokens SET permissions = replace(replace(permissions, '"sa:work:', '"sa:sites:work:'), '"sa:analysis:', '"sa:sites:analysis:');
UPDATE access_requests SET need = 'sa:sites:' || substr(need, 4) WHERE need LIKE 'sa:work:%' OR need LIKE 'sa:analysis:%';
