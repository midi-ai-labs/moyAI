ALTER TABLE sessions
ADD COLUMN model_parameters_json TEXT NOT NULL DEFAULT '{}';
