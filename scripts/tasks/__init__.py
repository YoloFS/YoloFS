from scripts.tasks.base import Task
from scripts.tasks.chmod import CHMOD_TASKS
from scripts.tasks.dir_copy import DIR_COPY_TASKS
from scripts.tasks.dir_create import DIR_CREATE_TASKS
from scripts.tasks.dir_delete import DIR_DELETE_TASKS
from scripts.tasks.dir_list import DIR_LIST_TASKS
from scripts.tasks.dir_move import DIR_RENAME_TASKS
from scripts.tasks.file_append import FILE_APPEND_TASKS
from scripts.tasks.file_clear import FILE_CLEAR_TASKS
from scripts.tasks.file_copy import FILE_COPY_TASKS
from scripts.tasks.file_create import FILE_CREATE_TASKS
from scripts.tasks.file_delete import FILE_DELETE_TASKS
from scripts.tasks.file_edit import FILE_EDIT_TASKS
from scripts.tasks.file_move import FILE_RENAME_TASKS
from scripts.tasks.file_overwrite import FILE_OVERWRITE_TASKS
from scripts.tasks.file_read import FILE_READ_TASKS
from scripts.tasks.glob import GLOB_TASKS
from scripts.tasks.glob_and_delete import GLOB_AND_DELETE_TASKS
from scripts.tasks.glob_and_read import GLOB_AND_READ_TASKS
from scripts.tasks.grep import GREP_TASKS
from scripts.tasks.special import SPECIAL_TASKS
from scripts.tasks.symlink_create import SYMLINK_CREATE_TASKS

TASKS: list[Task] = [
    *DIR_LIST_TASKS,
    *FILE_READ_TASKS,
    *FILE_APPEND_TASKS,
    *FILE_OVERWRITE_TASKS,
    *FILE_CLEAR_TASKS,
    *FILE_EDIT_TASKS,
    *FILE_CREATE_TASKS,
    *DIR_CREATE_TASKS,
    *FILE_DELETE_TASKS,
    *DIR_DELETE_TASKS,
    *FILE_RENAME_TASKS,
    *DIR_RENAME_TASKS,
    *FILE_COPY_TASKS,
    *DIR_COPY_TASKS,
    *CHMOD_TASKS,
    *SYMLINK_CREATE_TASKS,
    *GLOB_TASKS,
    *GLOB_AND_READ_TASKS,
    *GLOB_AND_DELETE_TASKS,
    *GREP_TASKS,
    *SPECIAL_TASKS,
]
