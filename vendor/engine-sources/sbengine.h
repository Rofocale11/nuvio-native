/* The torrent engine library's interface (libsbengine, loaded on demand by p2p.c). Every call
   returns at once; the app checks back each frame. Ids are 40-character lower-case info hashes. */
#ifndef SB_ENGINE_H
#define SB_ENGINE_H
#include <stdint.h>
#define SBE_API_VERSION 4
typedef struct { char path[512]; uint64_t size; } sbe_file;
typedef struct {
  uint32_t peers, seeds, known, connecting;
  uint64_t down_bps, up_bps, downloaded, disk_used, disk_capacity;
  uint64_t window, resident, freed, refetched;   /* the streaming window: its size, the payload kept, what was freed and fetched again */
  uint64_t mem_cache_used;                       /* the engine's own piece cache in RAM */
  uint32_t window_unsupported;                   /* the storage cannot free played pieces (no hole punching) */
  /* API 4: the playing stream's own numbers, so the transfer log can say where the bytes went:
     downloaded is engine-wide, delivered is what the player actually read */
  uint32_t scheduled_pieces;                     /* pieces the stream's current schedule asks libtorrent for */
  uint32_t active_demands;                       /* player HTTP reads in flight */
  uint64_t delivered;                            /* bytes served to the player since the stream opened */
  uint64_t piece_bytes;                          /* the torrent's piece length (pieces are the eviction unit) */
  uint64_t primary_demand_start;                 /* file-relative offset of the newest player read */
  uint64_t secondary_demand_start;               /* a second live read, if one exists (0 otherwise) */
} sbe_stats;
typedef struct {
  int         (*api)(void);
  const char *(*version)(void);
  /* 0 = running. windowBytes: payload kept on storage around the playhead; memCacheBytes: verified pieces in RAM;
     segmentBytes > 0: the payload as segment files of about that size, deleted as the window moves (storage that
     cannot free bytes inside a file, FAT32), 0: each torrent's own files with holes punched */
  int         (*start)(const char *stateDir, const char *cacheDir, uint64_t windowBytes, uint64_t uploadBps, uint64_t memCacheBytes, uint64_t segmentBytes);
  void        (*stop)(void);
  int         (*open)(const char *magnet, const char *ih);           /* adds the torrent once; 0 = ok */
  int         (*files)(const char *ih, sbe_file *out, int max);      /* the file count, -1 metadata not ready, -2 failed (see error) */
  int         (*stream)(const char *ih, uint32_t idx, char *url, int cap); /* 1 ready (url filled), 0 preparing, -1 failed */
  const char *(*error)(const char *ih);
  int         (*stats)(sbe_stats *out);
  void        (*limits)(uint64_t downBps, uint64_t upBps);           /* 0 = unlimited */
  void        (*remove)(const char *ih);                             /* the torrent and its payload go */
  void        (*remove_all)(void);
  void        (*window)(uint64_t bytes);                             /* a new window size while running */
  int         (*setting)(const char *name, int value);               /* one libtorrent setting by name (int or bool); 0 = applied */
  /* the payload moves to another folder (the old one vanished with its drive, or a better one is chosen between films):
     torrents download again there, streams keep their address; 0 = queued */
  int         (*relocate)(const char *cacheDir, uint64_t segmentBytes);
  int         (*restored)(const char *ih);                           /* 1 = metadata came from a kept resume file */
} sbe_api;
#endif
