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
#include "CallsRepository.h"
#include "Utils/StringConverter.h"

CallsRepository::CallsRepository(SQLite::Database& db)
    : m_db(db)
{
}

void CallsRepository::BeginTransaction()
{
    m_db.exec("BEGIN TRANSACTION;");
}

void CallsRepository::Commit()
{
    m_db.exec("COMMIT;");
}

void CallsRepository::Rollback()
{
    m_db.exec("ROLLBACK;");
}

int CallsRepository::Add(const Call& call)
{
    SQLite::Statement query(m_db,
        "INSERT INTO call_log "
        "(callId, number, name, type, time, duration, info"
        ") VALUES (?, ?, ?, ?, ?, ?, ?"
        ");");

    BindCallFields(query, call, false);
    query.exec();

    return static_cast<int>(m_db.getLastInsertRowid());
}

void CallsRepository::Update(const Call& call)
{
    SQLite::Statement query(m_db,
        "UPDATE call_log SET "
        "callId=?, number=?, name=?, type=?, time=?, duration=?, info=? "
        "WHERE id=?;");

    BindCallFields(query, call, true);
    query.exec();
}

void CallsRepository::Delete(int id)
{
    SQLite::Statement query(m_db,
        "DELETE FROM call_log WHERE id=?;");
    query.bind(1, id);
    query.exec();
}

void CallsRepository::DeleteByIds(std::vector<int> ids)
{
    std::string str;
    for (size_t i = 0; i < ids.size(); ++i)
    {
        if (i > 0)
            str += ",";
        str += std::to_string(ids[i]);
    }
    SQLite::Statement query(m_db,
        "DELETE FROM call_log WHERE id IN (" + str + ");");
    query.exec();
}

void CallsRepository::DeleteAll()
{
    m_db.exec("DELETE FROM call_log;");
}

std::unique_ptr<Call> CallsRepository::GetById(int id)
{
    SQLite::Statement query(m_db,
        "SELECT * FROM call_log WHERE id=?;");
    query.bind(1, id);

    if (query.executeStep())
        return MapQueryToCall(query);

    return nullptr;
}

std::unique_ptr<Call> CallsRepository::GetByCallId(const CString& callId)
{
    SQLite::Statement query(m_db,
        "SELECT * FROM call_log WHERE callId=?;");
    query.bind(1, CStringToUtf8(callId));

    if (query.executeStep())
        return MapQueryToCall(query);

    return nullptr;
}

std::vector<std::unique_ptr<Call>> CallsRepository::GetAll(int limit)
{
    std::string str;
    if (limit) {
        str = " LIMIT " + std::to_string(limit);
    }
    std::vector<std::unique_ptr<Call>> result;
    SQLite::Statement query(m_db, "SELECT * FROM call_log" + str + ";");
    while (query.executeStep())
    {
        result.push_back(MapQueryToCall(query));
    }
    return result;
}

std::unique_ptr<Call> CallsRepository::MapQueryToCall(SQLite::Statement& query)
{
    auto call = std::make_unique<Call>();
    call->id = query.getColumn("id").getInt();
    call->callId = Utf8ToCString(query.getColumn("callId").getString());
    call->number = Utf8ToCString(query.getColumn("number").getString());
    call->name = Utf8ToCString(query.getColumn("name").getString());
    call->type = query.getColumn("type").getInt();
    call->time = query.getColumn("time").getInt64();
    call->duration = query.getColumn("duration").getInt();
    call->info = Utf8ToCString(query.getColumn("info").getString());
    return call;
}

void CallsRepository::BindCallFields(SQLite::Statement& query, const Call& call, bool includeId)
{
    int index = 1;

    query.bind(index++, CStringToUtf8(call.callId));
    query.bind(index++, CStringToUtf8(call.number));
    query.bind(index++, CStringToUtf8(call.name));
    query.bind(index++, call.type);
    query.bind(index++, call.time);
    query.bind(index++, call.duration);
    query.bind(index++, CStringToUtf8(call.info));

    if (includeId) {
        query.bind(index++, call.id);
    }
}
