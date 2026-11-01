-- The sites business moved to sites_panel, which starts from a copy of this database: its
-- journal, projections, places, pricing, experiments, bookings, PostHog outbox and invoices go.
-- Children before their parents, so no drop trips a foreign key.
DROP VIEW reporting_funnel_daily;
DROP VIEW reporting_leads;
DROP VIEW reporting_ingest_daily;
DROP VIEW reporting_payments;
DROP VIEW reporting_calls;
DROP TABLE calls;
DROP TABLE payments;
DROP TABLE booking_events;
DROP TABLE bookings;
DROP TABLE booking_sync;
DROP TABLE posthog_outbox;
DROP TABLE experiments;
DROP TABLE leads;
DROP TABLE place_changes;
DROP TABLE place_settings;
DROP TABLE places;
DROP TABLE pricing_changes;
DROP TABLE pricing;
DROP TABLE brand_locales;
DROP TABLE invoices;
DROP TABLE invoice_cursor;
DROP TABLE events;
