ALTER TABLE users ADD COLUMN color TEXT NOT NULL DEFAULT '#808080';
UPDATE users SET color='#'||printf('%02x%02x%02x',abs(random()%128)+32,abs(random()%128)+32,abs(random()%128)+32);
CREATE TRIGGER default_nickname_color AFTER INSERT ON users BEGIN UPDATE users SET color='#'||printf('%02x%02x%02x',abs(random()%128)+32,abs(random()%128)+32,abs(random()%128)+32) WHERE id=new.id; END;
PRAGMA user_version=3;
