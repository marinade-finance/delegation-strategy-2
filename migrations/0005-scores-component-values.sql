alter table scores add column "component_values" text[] NOT NULL default '{NULL,NULL,NULL}'::text[];
alter table scores alter column "component_values" drop default;
