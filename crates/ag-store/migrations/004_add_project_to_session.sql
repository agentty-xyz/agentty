ALTER TABLE session ADD COLUMN project_id INTEGER REFERENCES project(id);
