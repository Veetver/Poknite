ALTER TABLE users ADD COLUMN disabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channels ADD COLUMN closed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channels ADD COLUMN kind TEXT NOT NULL DEFAULT 'channel' CHECK(kind IN ('channel','direct'));
ALTER TABLE channels ADD COLUMN user_lo INTEGER REFERENCES users(id);
ALTER TABLE channels ADD COLUMN user_hi INTEGER REFERENCES users(id);
CREATE UNIQUE INDEX direct_pair ON channels(user_lo,user_hi) WHERE kind='direct';
ALTER TABLE messages ADD COLUMN mentions TEXT NOT NULL DEFAULT '[]';
CREATE TABLE roles(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL UNIQUE,allow TEXT NOT NULL,deny TEXT NOT NULL,disabled INTEGER NOT NULL DEFAULT 0);
CREATE TABLE user_roles(user_id INTEGER NOT NULL REFERENCES users(id),role_id INTEGER NOT NULL REFERENCES roles(id),PRIMARY KEY(user_id,role_id));
CREATE TABLE channel_rules(channel_id INTEGER NOT NULL REFERENCES channels(id),role_id INTEGER NOT NULL REFERENCES roles(id),allow TEXT NOT NULL,deny TEXT NOT NULL,PRIMARY KEY(channel_id,role_id));
INSERT INTO roles(id,name,allow,deny) VALUES
(1,'Администратор','["profile","contacts","direct","read","send","mention","manage_channel","users","channels","devices","invitations","roles"]','[]'),
(2,'Участник','["profile","contacts","direct","read","send","mention"]','[]'),
(3,'Читатель','["profile","contacts","read"]','["send","mention","direct"]');
INSERT INTO user_roles SELECT id,2 FROM users;
CREATE TRIGGER default_user_role AFTER INSERT ON users BEGIN INSERT INTO user_roles VALUES(new.id,2); END;
PRAGMA user_version=2;
