#ifndef REELGET_H
#define REELGET_H

#ifdef __cplusplus
extern "C" {
#endif

typedef void (*reelget_progress_cb)(const char *event_json, void *user_data);

const char *reelget_version(void);

void reelget_free_string(char *ptr);

char *reelget_info_json(
    const char *url,
    const char *cookies_file,
    const char *raw_cookie
);

char *reelget_download_json(
    const char *url,
    const char *output_dir,
    const char *quality,
    const char *cookies_file,
    const char *raw_cookie,
    reelget_progress_cb progress_cb,
    void *user_data
);

char *reelget_download_clip_json(
    const char *url,
    const char *output_dir,
    const char *quality,
    const char *from_time,
    const char *to_time,
    const char *cookies_file,
    const char *raw_cookie,
    reelget_progress_cb progress_cb,
    void *user_data
);

char *reelget_download_mp3_json(
    const char *url,
    const char *output_dir,
    const char *bitrate,
    const char *from_time,
    const char *to_time,
    const char *cookies_file,
    const char *raw_cookie,
    reelget_progress_cb progress_cb,
    void *user_data
);

#ifdef __cplusplus
}
#endif

#endif /* REELGET_H */
