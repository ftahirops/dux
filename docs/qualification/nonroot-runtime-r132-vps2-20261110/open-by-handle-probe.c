#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s FILE MOUNT_ROOT\n", argv[0]);
        return 2;
    }

    const unsigned int handle_capacity = 128;
    struct file_handle *handle = calloc(
        1, sizeof(*handle) + handle_capacity
    );
    if (!handle) {
        perror("calloc file_handle");
        return 1;
    }
    handle->handle_bytes = handle_capacity;
    int mount_id = 0;
    if (name_to_handle_at(AT_FDCWD, argv[1], handle, &mount_id, 0) != 0) {
        fprintf(stderr, "name_to_handle_at(%s): %s\n", argv[1], strerror(errno));
        free(handle);
        return 1;
    }

    int mount_fd = open(argv[2], O_RDONLY | O_DIRECTORY);
    if (mount_fd < 0) {
        perror("open mount root");
        free(handle);
        return 1;
    }
    int file_fd = open_by_handle_at(mount_fd, handle, O_RDONLY);
    if (file_fd < 0) {
        fprintf(stderr, "open_by_handle_at: %s\n", strerror(errno));
        close(mount_fd);
        free(handle);
        return 1;
    }

    char contents[128];
    ssize_t length = read(file_fd, contents, sizeof(contents));
    if (length < 0) {
        perror("read handle-opened file");
        close(file_fd);
        close(mount_fd);
        free(handle);
        return 1;
    }
    if (write(STDOUT_FILENO, contents, (size_t)length) != length) {
        perror("write result");
        close(file_fd);
        close(mount_fd);
        free(handle);
        return 1;
    }
    fprintf(stderr, "handle_type=%d handle_bytes=%u mount_id=%d\n",
            handle->handle_type, handle->handle_bytes, mount_id);
    close(file_fd);
    close(mount_fd);
    free(handle);
    return 0;
}
