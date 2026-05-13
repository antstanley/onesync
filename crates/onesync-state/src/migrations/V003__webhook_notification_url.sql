-- M12 — add InstanceConfig.webhook_notification_url so the scheduler knows what URL to
-- pass to Graph /subscriptions as notificationUrl. NULL when no tunnel is configured;
-- the scheduler then skips subscription registration even for webhook_enabled pairs.

ALTER TABLE instance_config ADD COLUMN webhook_notification_url TEXT;
