/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#include "stdafx.h"
#include "Database.h"

#pragma comment(lib, "SQLiteCpp.lib")
#pragma comment(lib, "sqlite3.lib")

Database::Database(const std::string& filename)
{
    m_db = std::make_unique<SQLite::Database>(
        filename,
        SQLite::OPEN_READWRITE | SQLite::OPEN_CREATE
    );
    Get().exec("PRAGMA busy_timeout=5000;");
}

SQLite::Database& Database::Get()
{
    return *m_db;
}

void Database::InitCalls()
{
    Get().exec(
        "CREATE TABLE IF NOT EXISTS call_log ("
        "id INTEGER PRIMARY KEY AUTOINCREMENT"
        ",callId TEXT NOT NULL"
        ",number TEXT NOT NULL"
        ",name TEXT NOT NULL"
        ",type INTEGER"
        ",time INTEGER"
        ",duration INTEGER"
        ",info TEXT NOT NULL"
        ");"
    );
    Get().exec("CREATE UNIQUE INDEX IF NOT EXISTS idx_call_log_callId_unique ON call_log(callId);");

    Get().exec("DELETE FROM call_log\
        WHERE(SELECT COUNT(*) FROM call_log) > 1000\
        AND id <= (\
            SELECT id FROM call_log\
            ORDER BY id DESC\
            LIMIT 1 OFFSET 1000\
            );"
    );

}

void Database::InitContacts()
{
}
