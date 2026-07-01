-- Thread rooms replace hand-curated rooms entirely. A room IS a Bluesky
-- thread; the artist's own threads are the museum's first-class rooms,
-- other authors' registered threads hang as guest rooms.
DROP TABLE room_specimens;

DROP TABLE rooms;

ALTER TABLE guest_rooms RENAME TO thread_rooms;
