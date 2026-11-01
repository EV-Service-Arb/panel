UPDATE telegram_links SET permissions = replace(replace(permissions, '"sa:sites:work:', '"sa:work:'), '"sa:sites:analysis:', '"sa:analysis:')
	WHERE permissions IS NOT NULL;
UPDATE telegram_link_tokens SET permissions = replace(replace(permissions, '"sa:sites:work:', '"sa:work:'), '"sa:sites:analysis:', '"sa:analysis:');
UPDATE access_requests SET need = 'sa:' || substr(need, 10) WHERE need LIKE 'sa:sites:%';
