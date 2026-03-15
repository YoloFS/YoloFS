from scripts.microtasks.base import Task
from scripts.microtasks.chmod_file import CHMOD_FILE_TASKS
from scripts.microtasks.copy_dir import COPY_DIR_TASKS
from scripts.microtasks.create_dir import CREATE_DIR_TASKS
from scripts.microtasks.delete_dir import DELETE_DIR_TASKS
from scripts.microtasks.list_dir import LIST_DIR_TASKS
from scripts.microtasks.move_dir import MOVE_DIR_TASKS
from scripts.microtasks.append_file import APPEND_FILE_TASKS
from scripts.microtasks.clear_file import CLEAR_FILE_TASKS
from scripts.microtasks.copy_file import COPY_FILE_TASKS
from scripts.microtasks.create_file import CREATE_FILE_TASKS
from scripts.microtasks.delete_file import DELETE_FILE_TASKS
from scripts.microtasks.edit_file import EDIT_FILE_TASKS
from scripts.microtasks.move_file import MOVE_FILE_TASKS
from scripts.microtasks.overwrite_file import OVERWRITE_FILE_TASKS
from scripts.microtasks.read_file import READ_FILE_TASKS
from scripts.microtasks.glob import GLOB_TASKS
from scripts.microtasks.glob_and_delete import GLOB_AND_DELETE_TASKS
from scripts.microtasks.glob_and_read import GLOB_AND_READ_TASKS
from scripts.microtasks.grep import GREP_TASKS
from scripts.microtasks.special import SPECIAL_TASKS
from scripts.microtasks.create_symlink import CREATE_SYMLINK_TASKS

MICROTASKS: list[Task] = [
    *LIST_DIR_TASKS,
    *READ_FILE_TASKS,
    *APPEND_FILE_TASKS,
    *OVERWRITE_FILE_TASKS,
    *CLEAR_FILE_TASKS,
    *EDIT_FILE_TASKS,
    *CREATE_FILE_TASKS,
    *CREATE_DIR_TASKS,
    *DELETE_FILE_TASKS,
    *DELETE_DIR_TASKS,
    *MOVE_FILE_TASKS,
    *MOVE_DIR_TASKS,
    *COPY_FILE_TASKS,
    *COPY_DIR_TASKS,
    *CHMOD_FILE_TASKS,
    *CREATE_SYMLINK_TASKS,
    *GLOB_TASKS,
    *GLOB_AND_READ_TASKS,
    *GLOB_AND_DELETE_TASKS,
    *GREP_TASKS,
    *SPECIAL_TASKS,
]
